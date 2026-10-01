//! Replay retained originals only into the session files. These workers have no
//! access to playback, translation, command activation or physical devices.
use super::*;
use crate::history::{HistoryFrame, HistorySnapshot};
use std::collections::VecDeque;

const MAX_PENDING_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_RECORDS: usize = 8192;

pub(super) fn validate_request(config: &AppConfig, seconds: u32) -> Result<()> {
    if seconds == 0 {
        return Ok(());
    }
    ensure!(
        config.history.enabled,
        "Enable recent audio history before including it in a session"
    );
    ensure!(
        seconds <= config.history.duration_secs,
        "The requested portion exceeds the configured history duration"
    );
    ensure!(
        config.recording.enabled || config.transcription.enabled,
        "History can only be included in sessions with recording or transcription enabled"
    );
    Ok(())
}

fn selected(config: &AppConfig, lane: RecordingLane) -> bool {
    match lane {
        RecordingLane::Microphone => {
            !config.microphone_uses_speaker()
                && ((config.recording.enabled && config.recording.microphone)
                    || (config.transcription.enabled && config.transcription.microphone))
        }
        RecordingLane::Speaker => {
            (config.recording.enabled && config.recording.speaker)
                || (config.transcription.enabled && config.transcription.speaker)
        }
    }
}

pub(super) fn select_sources(snapshot: &mut HistorySnapshot, config: &AppConfig, now: Instant) {
    snapshot.frames.retain(|frame| selected(config, frame.lane));
    snapshot.origin = snapshot
        .frames
        .iter()
        .map(|frame| {
            frame
                .captured_at
                .checked_sub(Duration::from_secs_f64(
                    frame.sample_count() as f64 / 16_000.0,
                ))
                .unwrap_or(frame.captured_at)
        })
        .min()
        .unwrap_or(now)
        .max(snapshot.origin);
    snapshot.included_secs = if snapshot.frames.is_empty() {
        0.0
    } else {
        now.saturating_duration_since(snapshot.origin).as_secs_f64()
    };
}

struct PendingGuard(Arc<AtomicBool>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Preserve the historical prefix while live STT runs independently. The text
/// backlog is bounded by both records and bytes; no audio callback waits on it.
pub(super) async fn write_transcript(
    writer: TranscriptWriter,
    mut live: mpsc::Receiver<TranscriptRecord>,
    config: AppConfig,
    snapshot: HistorySnapshot,
    pending: Arc<AtomicBool>,
) -> Result<()> {
    let guard = PendingGuard(pending);
    if !snapshot.frames.iter().any(|frame| match frame.lane {
        RecordingLane::Microphone => config.transcription.microphone,
        RecordingLane::Speaker => config.transcription.speaker,
    }) {
        drop(guard);
        drop(snapshot);
        drop(config);
        return writer.run(live).await;
    }
    let (output, output_rx) = mpsc::channel(128);
    // Keep ownership of the file task if this worker fails or is dropped.
    let mut writer_task =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(writer.run(output_rx)));
    let (history_tx, mut history_rx) = mpsc::channel(32);
    // Stop closes capture, but accepted history belongs to the file worker.
    // Cancel replay only when this worker ends, never with the device token.
    let replay_cancel = CancellationToken::new();
    let replay_guard = replay_cancel.clone().drop_guard();
    let seconds = snapshot.included_secs;
    let captured_at = chrono::Utc::now()
        - chrono::Duration::from_std(snapshot.origin.elapsed()).unwrap_or_default();
    let replay = replay(config, snapshot, history_tx, replay_cancel);
    tokio::pin!(replay);
    let mut backlog = VecDeque::new();
    let mut backlog_bytes = 0;
    let mut live_open = true;
    let mut history_open = true;
    let result = async {
        output.send(TranscriptRecord::Section(format!(
            "Recovered history: {seconds:.1} s. Audio origin: {}", captured_at.to_rfc3339()
        ))).await.context("Transcript file closed")?;
        let replay_result = loop {
            tokio::select! {
                result = &mut replay => break result,
                record = history_rx.recv(), if history_open => match record {
                    Some(record) => output.send(record).await.context("Transcript file closed")?,
                    None => history_open = false,
                },
                record = live.recv(), if live_open && backlog.len() < MAX_PENDING_RECORDS && backlog_bytes < MAX_PENDING_TEXT_BYTES - 64 * 1024 => match record {
                    Some(record) => {
                        backlog_bytes += record_size(&record)?;
                        backlog.push_back(record);
                    }
                    None => live_open = false,
                },
            }
        };
        while let Ok(record) = history_rx.try_recv() {
            output.send(record).await.context("Transcript file closed")?;
        }
        drop(guard);
        if let Err(error) = &replay_result {
            output.send(TranscriptRecord::Section(format!("Incomplete history: {error:#}")))
                .await.context("Transcript file closed")?;
        }
        output.send(TranscriptRecord::Section("From the session start".into()))
            .await.context("Transcript file closed")?;
        for record in backlog { output.send(record).await.context("Transcript file closed")?; }
        // A failed history prefix must not discard live finals still arriving
        // after the capture routes have stopped.
        while let Some(record) = live.recv().await {
            output.send(record).await.context("Transcript file closed")?;
        }
        replay_result
    }.await;
    drop(replay_guard);
    drop(output);
    let finalized = (&mut writer_task)
        .await
        .context("Transcript file interrupted")?;
    result.and(finalized)
}

fn record_size(record: &TranscriptRecord) -> Result<usize> {
    Ok(match record {
        TranscriptRecord::Text {
            text,
            metadata,
            received_at,
            ..
        } => {
            ensure!(text.len() <= 32768, "Transcript segment exceeds 32 KiB");
            128 + text.len() + received_at.len() + metadata.speaker.as_ref().map_or(0, String::len)
        }
        TranscriptRecord::Routed { record, .. } => {
            ensure!(
                !matches!(**record, TranscriptRecord::Routed { .. }),
                "Invalid nested transcript source"
            );
            32 + record_size(record)?
        }
        TranscriptRecord::Section(text) => 128 + text.len(),
        _ => 128,
    })
}

async fn replay(
    config: AppConfig,
    snapshot: HistorySnapshot,
    output: mpsc::Sender<TranscriptRecord>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut routes = JoinSet::new();
    for (lane, origin, selected, recognition) in [
        (
            RecordingLane::Microphone,
            TranscriptOrigin::Microphone,
            config.transcription.microphone,
            config.transcription.microphone_recognition.clone(),
        ),
        (
            RecordingLane::Speaker,
            TranscriptOrigin::Speaker,
            config.transcription.speaker,
            config.transcription.speaker_recognition.clone(),
        ),
    ] {
        if !selected {
            continue;
        }
        let frames: Vec<_> = snapshot
            .frames
            .iter()
            .filter(|frame| frame.lane == lane)
            .cloned()
            .collect();
        if frames.is_empty() {
            continue;
        }
        let profiles = config.transcription.providers.clone();
        let output = output.clone();
        let route_cancel = cancel.child_token();
        let clock = snapshot.origin;
        routes.spawn(async move {
            let provider = provider::stt::create(&recognition, &profiles)?;
            let session = provider::stt::session_config(&recognition, &profiles)?;
            let (audio_tx, audio_rx) = mpsc::channel(8);
            let input = feed_frames(frames, clock, audio_tx, route_cancel.clone());
            let model = recovery::transcribe(
                provider,
                session,
                audio_rx,
                output,
                origin,
                route_cancel.clone(),
            );
            tokio::try_join!(input, model)?;
            ensure!(
                !route_cancel.is_cancelled(),
                "History transcription stopped before completion"
            );
            Ok::<_, anyhow::Error>(())
        });
    }
    while let Some(result) = routes.join_next().await {
        result.context("History transcription interrupted")??;
    }
    Ok(())
}

/// Pad actual capture gaps, but suppress <=50ms scheduler jitter like the WAV
/// mixer. Provider offsets then refer to the same original session timeline.
async fn feed_frames(
    frames: Vec<HistoryFrame>,
    origin: Instant,
    audio: mpsc::Sender<Vec<i16>>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut next = 0u64;
    let mut reader = crate::history::HistoryReader::default();
    for frame in frames {
        let end = (frame
            .captured_at
            .saturating_duration_since(origin)
            .as_nanos()
            * 16000
            / 1_000_000_000) as u64;
        let candidate = end.saturating_sub(frame.sample_count() as u64);
        let start = if next > 0 && candidate <= next.saturating_add(800) {
            next
        } else {
            candidate
        };
        while next < start {
            let count = (start - next).min(16000) as usize;
            send_audio(&audio, vec![0; count], &cancel).await?;
            next += count as u64;
        }
        let samples = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("History transcription stopped before completion"),
            samples = frame.read_samples(&mut reader) => samples?,
        };
        for chunk in samples.chunks(16000) {
            send_audio(&audio, chunk.to_vec(), &cancel).await?;
            next += chunk.len() as u64;
        }
    }
    Ok(())
}
async fn send_audio(
    sender: &mpsc::Sender<Vec<i16>>,
    samples: Vec<i16>,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("History transcription stopped before completion"),
        sent = sender.send(samples) => sent.context("The provider closed the history input"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{history::HistoryBuffer, provider::TranscriptMetadata};

    #[tokio::test]
    async fn speaker_mirror_history_retains_one_original_lane_and_excludes_the_prior_physical_mic()
    {
        let mut config = AppConfig::default();
        config.audio.microphone_source = crate::config::MicrophoneSource::SpeakerOriginal;
        config.recording.enabled = true;
        config.transcription.enabled = true;
        let history = HistoryBuffer::new(&config.history);
        let now = Instant::now();
        history.push(RecordingLane::Microphone, &[123; 1600], now);
        history.push(RecordingLane::Speaker, &[456; 1600], now);
        let mut snapshot = history.snapshot(10, now);
        select_sources(&mut snapshot, &config, now);
        assert_eq!(snapshot.frames.len(), 1);
        assert_eq!(snapshot.frames[0].lane, RecordingLane::Speaker);
        assert_eq!(
            snapshot.frames[0]
                .read_samples(&mut crate::history::HistoryReader::default())
                .await
                .unwrap()
                .as_slice(),
            &[456; 1600]
        );
        config.recording.speaker = false;
        config.transcription.speaker = false;
        let mut snapshot = history.snapshot(10, now);
        select_sources(&mut snapshot, &config, now);
        assert!(snapshot.frames.is_empty());
    }

    #[test]
    fn history_requires_explicit_request_and_selected_file_features() {
        let mut config = AppConfig::default();
        config.recording.enabled = false;
        config.transcription.enabled = false;
        assert!(validate_request(&config, 0).is_ok());
        assert!(validate_request(&config, 600).is_err());
        config.recording.enabled = true;
        assert!(validate_request(&config, 600).is_ok());
        assert!(validate_request(&config, 601).is_err());
        config.history.enabled = false;
        assert!(validate_request(&config, 1).is_err());
        assert!(validate_request(&config, 0).is_ok());
    }

    #[tokio::test]
    async fn stt_history_preserves_real_capture_gaps_and_channel_order() {
        let buffer = HistoryBuffer::new(&Default::default());
        let now = Instant::now();
        let origin = now - Duration::from_secs(5);
        buffer.push(
            RecordingLane::Microphone,
            &[1000; 1600],
            origin + Duration::from_millis(100),
        );
        buffer.push(
            RecordingLane::Microphone,
            &[2000; 1600],
            origin + Duration::from_millis(400),
        );
        buffer.flush().await;
        let snapshot = buffer.snapshot(10, now);
        let (tx, mut rx) = mpsc::channel(1);
        let collect = async {
            let mut pcm = Vec::new();
            while let Some(frame) = rx.recv().await {
                pcm.extend(frame);
            }
            pcm
        };
        let (sent, pcm) = tokio::join!(
            feed_frames(snapshot.frames, origin, tx, CancellationToken::new()),
            collect
        );
        sent.unwrap();
        assert_eq!(pcm.len(), 6400);
        assert!(pcm[..1600].iter().all(|&v| v == 1000));
        assert!(pcm[1600..4800].iter().all(|&v| v == 0));
        assert!(pcm[4800..].iter().all(|&v| v == 2000));
    }

    #[tokio::test]
    async fn both_historical_lanes_share_one_mixed_wav_and_interleaved_transcript_clock() {
        // Responses are explicitly released in microphone/speaker/microphone
        // order. No real recognizer, capture device or scheduler delay is used.
        let (requests, mut pending_requests) = mpsc::channel(4);
        let app = axum::Router::new().route(
            "/inference",
            axum::routing::post(move |body: axum::body::Bytes| {
                let requests = requests.clone();
                async move {
                    let form = String::from_utf8_lossy(&body);
                    assert!(form.contains("name=\"translate\"\r\n\r\nfalse"));
                    let microphone = form.contains("name=\"language\"\r\n\r\npt");
                    assert!(microphone || form.contains("name=\"language\"\r\n\r\nen"));
                    let (reply, response) = tokio::sync::oneshot::channel::<&'static str>();
                    requests.send((microphone, reply)).await.unwrap();
                    axum::Json(serde_json::json!({"text": response.await.unwrap()}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.recording.enabled = true;
        config.transcription.enabled = true;
        config.transcription.directory = directory.path().to_string_lossy().into_owned();
        config.transcription.microphone_recognition.provider = "whisper".into();
        config.transcription.microphone_recognition.language = "pt".into();
        config.transcription.speaker_recognition.provider = "whisper".into();
        config.transcription.speaker_recognition.language = "en".into();
        config.transcription.providers.whisper.endpoint = endpoint;
        config.transcription.providers.whisper.segment_ms = 500;
        config.transcription.providers.whisper.silence_ms = 100;

        let now = Instant::now() - Duration::from_secs(2);
        let origin = now - Duration::from_secs(1);
        let buffer = HistoryBuffer::new(&config.history);
        for (lane, value, end_ms) in [
            (RecordingLane::Microphone, 1000, 500),
            (RecordingLane::Speaker, 3000, 500),
            (RecordingLane::Microphone, 2000, 1000),
        ] {
            buffer.push(lane, &[value; 8000], origin + Duration::from_millis(end_ms));
        }
        let mut snapshot = buffer.snapshot(10, now);
        select_sources(&mut snapshot, &config, now);
        assert_eq!(snapshot.origin, origin);
        assert_eq!(snapshot.included_secs, 1.0);
        assert_eq!(snapshot.frames.len(), 3);
        let recorder =
            SessionAudioRecorder::create(directory.path(), "combined", snapshot.origin, true, true)
                .await
                .unwrap();
        let writer = TranscriptWriter::create_merged(
            &config.transcription,
            "combined",
            "session",
            "Two lanes",
        )
        .await
        .unwrap();
        let (audio, audio_rx) = mpsc::channel(2);
        let (live, live_rx) = mpsc::channel(2);
        for (lane, origin, value, text) in [
            (
                RecordingLane::Microphone,
                TranscriptOrigin::Microphone,
                4000,
                "microfone ao vivo",
            ),
            (
                RecordingLane::Speaker,
                TranscriptOrigin::Speaker,
                6000,
                "saída ao vivo",
            ),
        ] {
            audio
                .send(AudioRecord {
                    lane,
                    samples: vec![value; 8000],
                    captured_at: snapshot.origin + Duration::from_millis(1500),
                })
                .await
                .unwrap();
            live.send(TranscriptRecord::Routed {
                origin,
                record: Box::new(TranscriptRecord::Text {
                    input: true,
                    text: text.into(),
                    metadata: TranscriptMetadata {
                        start_ms: Some(1000),
                        end_ms: Some(1500),
                        ..Default::default()
                    },
                    received_at: chrono::Utc::now().to_rfc3339(),
                }),
            })
            .await
            .unwrap();
        }
        drop(audio);
        drop(live);
        let frames = snapshot.frames.clone();
        let transcript = tokio::spawn(write_transcript(
            writer,
            live_rx,
            config,
            snapshot,
            Arc::new(AtomicBool::new(true)),
        ));
        let mut reader = crate::history::HistoryReader::default();
        let mut records = Vec::new();
        for frame in frames {
            records.push(AudioRecord {
                lane: frame.lane,
                samples: frame.read_samples(&mut reader).await.unwrap().to_vec(),
                captured_at: frame.captured_at,
            });
        }
        recorder.run_with_history(audio_rx, records).await.unwrap();

        let mut first_microphone = None;
        let mut first_speaker = None;
        for _ in 0..2 {
            let (microphone, reply) =
                tokio::time::timeout(Duration::from_secs(3), pending_requests.recv())
                    .await
                    .unwrap()
                    .unwrap();
            let slot = if microphone {
                &mut first_microphone
            } else {
                &mut first_speaker
            };
            assert!(slot.replace(reply).is_none());
        }
        let text_path = directory.path().join("combined.txt");
        async fn wait_for_text(path: &std::path::Path, expected: &str) {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !tokio::fs::read_to_string(path)
                    .await
                    .unwrap()
                    .contains(expected)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        first_microphone
            .unwrap()
            .send("microfone histórico 1")
            .unwrap();
        let (microphone, second_microphone) =
            tokio::time::timeout(Duration::from_secs(3), pending_requests.recv())
                .await
                .unwrap()
                .unwrap();
        assert!(microphone);
        let pending_text = tokio::fs::read_to_string(&text_path).await.unwrap();
        assert!(
            !pending_text.contains("microfone histórico 1"),
            "An unconfirmed finite window must not persist partial text"
        );
        second_microphone.send("microfone histórico 2").unwrap();
        wait_for_text(&text_path, "microfone histórico 2").await;
        first_speaker.unwrap().send("saída histórica").unwrap();
        wait_for_text(&text_path, "saída histórica").await;
        tokio::time::timeout(Duration::from_secs(3), transcript)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.abort();

        let mut wav = hound::WavReader::open(directory.path().join("combined.wav")).unwrap();
        assert_eq!(wav.spec().channels, 1);
        assert_eq!(wav.spec().sample_rate, 16_000);
        let pcm = wav.samples::<i16>().map(Result::unwrap).collect::<Vec<_>>();
        assert_eq!(pcm.len(), 24_000);
        assert!(pcm[..8000].iter().all(|&sample| sample == 2000));
        assert!(pcm[8000..16000].iter().all(|&sample| sample == 1000));
        assert!(pcm[16000..].iter().all(|&sample| sample == 5000));
        let text = tokio::fs::read_to_string(text_path).await.unwrap();
        let lines = [
            "[microphone] [audio +0.000–0.500s] microfone histórico 1",
            "[microphone] [audio +0.500–1.000s] microfone histórico 2",
            "[received output] [audio +0.000–0.500s] saída histórica",
            "[microphone] [audio +1.000–1.500s] microfone ao vivo",
            "[received output] [audio +1.000–1.500s] saída ao vivo",
        ];
        let positions = lines.map(|line| {
            assert_eq!(
                text.matches(line).count(),
                1,
                "missing or duplicated result: {line}\n{text}"
            );
            text.find(line).unwrap()
        });
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn only_selected_recording_or_transcription_sources_are_recovered() {
        let buffer = HistoryBuffer::new(&Default::default());
        let now = Instant::now();
        buffer.push(
            RecordingLane::Microphone,
            &[1; 1600],
            now - Duration::from_secs(2),
        );
        buffer.push(
            RecordingLane::Speaker,
            &[2; 1600],
            now - Duration::from_secs(1),
        );
        let mut config = AppConfig::default();
        config.recording.enabled = true;
        config.recording.microphone = false;
        let mut snapshot = buffer.snapshot(10, now);
        select_sources(&mut snapshot, &config, now);
        assert_eq!(snapshot.frames.len(), 1);
        assert_eq!(snapshot.frames[0].lane, RecordingLane::Speaker);
        assert!((snapshot.included_secs - 1.1).abs() < 0.0001);
        assert!(buffer.snapshot(0, now).frames.is_empty());
        assert!(
            !buffer.snapshot(10, now).frames.is_empty(),
            "starting never consumes the safety buffer"
        );
    }

    #[tokio::test]
    async fn recording_only_history_is_released_while_other_lane_keeps_transcribing() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.transcription.enabled = true;
        config.transcription.microphone = false;
        config.transcription.speaker = true;
        config.transcription.directory = directory.path().to_str().unwrap().into();
        let buffer = HistoryBuffer::new(&config.history);
        let now = Instant::now();
        buffer.push(
            RecordingLane::Microphone,
            &[1000; 1600],
            now - Duration::from_secs(1),
        );
        let snapshot = buffer.snapshot(10, now);
        let retained = Arc::downgrade(&snapshot.frames[0].samples);
        drop(buffer);
        let writer =
            TranscriptWriter::create_merged(&config.transcription, "release", "session", "Test")
                .await
                .unwrap();
        let (live, rx) = mpsc::channel(1);
        let pending = Arc::new(AtomicBool::new(true));
        let task = tokio::spawn(write_transcript(
            writer,
            rx,
            config,
            snapshot,
            pending.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while pending.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            retained.upgrade().is_none(),
            "unused PCM must not live until the transcript closes"
        );
        assert!(!task.is_finished());
        drop(live);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn history_failure_preserves_accepted_live_text_and_reports_partial_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/inference",
            axum::routing::post(|| async { axum::http::StatusCode::BAD_REQUEST }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = AppConfig::default();
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        config.transcription.directory = directory.path().to_str().unwrap().into();
        config.transcription.microphone_recognition.provider =
            "unsupported-fixture-provider".into();
        config.transcription.providers.whisper.endpoint = endpoint;
        let buffer = HistoryBuffer::new(&config.history);
        let now = Instant::now();
        buffer.push(
            RecordingLane::Microphone,
            &[5000; 8000],
            now - Duration::from_secs(1),
        );
        let snapshot = buffer.snapshot(10, now);
        let writer = TranscriptWriter::create_merged(
            &config.transcription,
            "failed-history",
            "session",
            "Test",
        )
        .await
        .unwrap();
        let (live, rx) = mpsc::channel(4);
        live.send(TranscriptRecord::Text {
            input: true,
            text: "resultado ao vivo já aceito".into(),
            metadata: Default::default(),
            received_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();
        let pending = Arc::new(AtomicBool::new(true));
        let final_text = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            live.send(TranscriptRecord::Text {
                input: true,
                text: "Final original received after capture stopped".into(),
                metadata: Default::default(),
                received_at: chrono::Utc::now().to_rfc3339(),
            })
            .await
            .unwrap();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            write_transcript(writer, rx, config, snapshot, pending.clone()),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        final_text.await.unwrap();
        assert!(!pending.load(Ordering::Acquire));
        let text = std::fs::read_to_string(directory.path().join("failed-history.txt")).unwrap();
        assert!(text.contains("Incomplete history:"));
        assert!(text.contains("resultado ao vivo já aceito"));
        assert!(text.contains("Final original received after capture stopped"));
        server.abort();
    }

    #[tokio::test]
    async fn closing_live_capture_keeps_history_pending_until_its_result_precedes_live_text() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let arrived = entered.clone();
        let proceed = release.clone();
        let app = axum::Router::new().route(
            "/inference",
            axum::routing::post(move |body: axum::body::Bytes| {
                let arrived = arrived.clone();
                let proceed = proceed.clone();
                async move {
                    let form = String::from_utf8_lossy(&body);
                    assert!(form.contains("name=\"translate\"\r\n\r\nfalse"));
                    assert!(form.contains("name=\"language\"\r\n\r\npt"));
                    arrived.notify_one();
                    proceed.notified().await;
                    axum::Json(serde_json::json!({"text":"fala recuperada"}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.transcription.enabled = true;
        config.transcription.speaker = false;
        config.transcription.timestamps = true;
        config.transcription.directory = directory.path().to_str().unwrap().into();
        config.transcription.microphone_recognition.provider = "whisper".into();
        config.transcription.microphone_recognition.language = "pt".into();
        config.transcription.providers.whisper.endpoint = endpoint;
        config.transcription.providers.whisper.segment_ms = 500;
        config.transcription.providers.whisper.silence_ms = 100;
        let buffer = HistoryBuffer::new(&config.history);
        let now = Instant::now();
        buffer.push(
            RecordingLane::Microphone,
            &[5000; 8000],
            now - Duration::from_secs(1),
        );
        let snapshot = buffer.snapshot(10, now);
        let writer =
            TranscriptWriter::create_merged(&config.transcription, "history", "session", "Test")
                .await
                .unwrap();
        let (live, rx) = mpsc::channel(4);
        let pending = Arc::new(AtomicBool::new(true));
        let mut task = tokio::spawn(write_transcript(
            writer,
            rx,
            config,
            snapshot,
            pending.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        live.send(TranscriptRecord::Routed {
            origin: TranscriptOrigin::Microphone,
            record: Box::new(TranscriptRecord::Text {
                input: true,
                text: "fala ao vivo".into(),
                received_at: chrono::Utc::now().to_rfc3339(),
                metadata: TranscriptMetadata {
                    start_ms: Some(1500),
                    end_ms: Some(1800),
                    ..Default::default()
                },
            }),
        })
        .await
        .unwrap();
        // Closing accepted live input is the file worker's Stop boundary.
        // It cannot cancel the separately owned, in-flight history request.
        drop(live);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut task)
                .await
                .is_err()
        );
        assert!(pending.load(Ordering::Acquire));
        assert!(
            !std::fs::read_to_string(directory.path().join("history.txt"))
                .unwrap()
                .contains("fala ao vivo")
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!pending.load(Ordering::Acquire));
        let text = std::fs::read_to_string(directory.path().join("history.txt")).unwrap();
        assert!(text.find("fala recuperada").unwrap() < text.find("fala ao vivo").unwrap());
        assert!(text.contains("[audio +0.000–0.500s]"));
        assert!(text.contains("[audio +1.500–1.800s]"));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        server.abort();
    }
}
