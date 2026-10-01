//! Session processing is separate from device capture and original playback.
use super::*;
#[cfg(test)]
use futures_util::FutureExt;

pub(super) async fn run_route(
    name: &'static str,
    cfg: AppConfig,
    route: RouteConfig,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    io: RouteIo,
) -> Result<()> {
    let RouteIo {
        #[cfg(test)]
        recognition,
        audio: mut audio_tx,
        origin,
        mut capture_changes,
        mut playback_changes,
        history,
        retained,
    } = io;
    if !wait_for_devices(&mut capture_changes, &mut playback_changes, &cancel).await {
        return Ok(());
    }
    let translating = route.enabled;
    metrics.original_mode.store(!translating, Ordering::Relaxed);
    let handle = crate::execution::audio_handle()?;
    let capture_device = capture_changes.borrow().clone();
    let playback_device = playback_changes.borrow().clone();
    let (rate, channels) = if translating {
        audio::original_format(&capture_device, &playback_device)
            .await
            .unwrap_or((INPUT_RATE, 1))
    } else {
        (48_000, 2)
    };
    let frame_ms = [10, 20, 40, 100]
        .into_iter()
        .find(|ms| (rate * ms) % 1000 == 0)
        .unwrap_or(100);
    let options = AudioOptions {
        sample_rate: rate,
        channels,
        frame_ms,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: if translating {
            cfg.audio.capture_queue_ms.max(frame_ms)
        } else {
            80.max(frame_ms)
        },
    };
    let (sidecar_tx, mut captured_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).clamp(1, 100) as usize);
    // Metrics survive endpoint reactivation. Previous activations already
    // reported their losses; observe only new losses from the workers below.
    let copy_losses = metrics.audio.sidecar_dropped_frames.load(Ordering::Relaxed);
    let capture_losses = metrics.audio.capture_lost_frames.load(Ordering::Relaxed);
    let mut audio_jobs = JoinSet::new();
    if translating {
        let (tx, rx) = mpsc::channel((options.queue_ms / frame_ms).max(1) as usize);
        let token = cancel.clone();
        let stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                audio::switching::capture(
                    &capture_device,
                    options,
                    tx,
                    capture_changes,
                    token,
                    stats,
                )
                .await
            },
            &handle,
        );
        audio_jobs.spawn_on(
            forward_processing(rx, sidecar_tx, cancel.clone(), metrics.audio.clone(), true),
            &handle,
        );
    } else {
        let token = cancel.clone();
        let stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                audio::passthrough::run_route_with_sidecar(
                    audio::passthrough::RouteDevices {
                        capture: capture_changes,
                        playback: playback_changes,
                    },
                    options,
                    token,
                    stats,
                    sidecar_tx,
                )
                .await
            },
            &handle,
        );
    }
    metrics.state(if translating {
        "running"
    } else {
        "passthrough"
    });
    let mut originals = OriginalSidecar {
        speech: audio::speech::SpeechTap::new(),
        history: &history,
        recording: &mut audio_tx,
        #[cfg(test)]
        recognition: recognition.as_ref(),
        retained: retained.as_deref(),
        origin,
        metrics: &metrics,
        copy_losses,
        capture_losses,
    };
    let result: Result<()> = async {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Ok(()),
                completed = audio_jobs.join_next() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    match completed {
                        Some(Ok(Err(error))) => break Err(error),
                        _ => bail!("An audio component ended unexpectedly"),
                    }
                }
                frame = captured_rx.recv() => {
                    let Some(original) = frame else { bail!("Audio capture ended"); };
                    if let Some(frame) = originals.retain(&original, false) {
                        metrics.input_level.store(rms(&frame.samples).to_bits(), Ordering::Relaxed);
                    }
                    if let Some(retained) = &retained {
                        metrics.recovery("original audio storage", retained.storage_recovering());
                    }
                    if retained.as_ref().is_some_and(|retained| retained.capture_requires_stop()) {
                        bail!("Original retention needs attention; capture stopped while accepted audio remains retained");
                    }
                }
            }
        }
    }.await;
    cancel.cancel();
    // Only device ownership is bounded. Session-owned readers keep every
    // retained original and finalize independently after this capture closes.
    let mut cleanup = Ok(());
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        let mut open = true;
        while !audio_jobs.is_empty() {
            tokio::select! {
                completed = audio_jobs.join_next() => if let Some(completed) = completed {
                    let result = completed.context("Capture task interrupted").and_then(|r| r);
                    if result.is_err() && cleanup.is_ok() { cleanup = result; }
                },
                frame = captured_rx.recv(), if open => match frame {
                    Some(frame) => { originals.retain(&frame, false); },
                    None => open = false,
                }
            }
        }
    })
    .await;
    if stopped.is_err() {
        audio_jobs.abort_all();
        if let Some(retained) = &retained {
            retained.mark_unrecoverable(
                "Capture shutdown timed out; the final device queue could not be verified",
            );
        }
        cleanup = Err(anyhow!("Timed out while stopping audio capture"));
    }
    originals.drain(&mut captured_rx);
    drop(originals);
    drop(audio_tx);
    #[cfg(test)]
    drop(recognition);
    metrics.input_level.store(0, Ordering::Relaxed);
    metrics.state(if result.is_ok() && cleanup.is_ok() {
        "stopped"
    } else {
        "error"
    });
    result
        .and(cleanup)
        .with_context(|| format!("Stream {name}"))
}

/// Owns only original side effects. It cannot send translation or playback, so
/// the same delivery path remains safe after virtual-device selection ends.
struct OriginalSidecar<'a> {
    speech: audio::speech::SpeechTap,
    history: &'a crate::history::HistoryBuffer,
    recording: &'a mut Option<mpsc::Sender<AudioRecord>>,
    #[cfg(test)]
    recognition: Option<&'a recognition::Sink>,
    retained: Option<&'a retained::RetainedSession>,
    origin: TranscriptOrigin,
    metrics: &'a RouteMetrics,
    copy_losses: u64,
    capture_losses: u64,
}

impl OriginalSidecar<'_> {
    fn has_recognition(&self) -> bool {
        #[cfg(test)]
        {
            self.recognition.is_some()
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn report_incomplete(&self, reason: &str) {
        self.metrics.report_processing_error(reason);
        if let Some(retained) = self.retained {
            retained.mark_incomplete(reason);
        }
    }

    fn report_missing_originals(&self, reason: &str) {
        self.metrics.report_processing_error(reason);
        if let Some(retained) = self.retained {
            retained.mark_unrecoverable(reason);
        }
    }

    fn observe_copy_losses(&mut self) {
        let losses = self
            .metrics
            .audio
            .sidecar_dropped_frames
            .load(Ordering::Relaxed);
        if losses > self.copy_losses
            && (self.recording.is_some() || self.has_recognition() || self.retained.is_some())
        {
            self.report_missing_originals(
                "Original audio was lost before session retention; those missing frames cannot be recovered",
            );
        }
        self.copy_losses = losses;
        let capture_losses = self
            .metrics
            .audio
            .capture_lost_frames
            .load(Ordering::Relaxed);
        if capture_losses > self.capture_losses
            && (self.recording.is_some() || self.has_recognition() || self.retained.is_some())
        {
            self.report_missing_originals(
                "Captured original audio was discarded before session processing; those missing frames cannot be recovered",
            );
        }
        self.capture_losses = capture_losses;
    }

    fn retain(
        &mut self,
        original: &audio::OriginalFrame,
        needs_translation: bool,
    ) -> Option<audio::PcmFrame> {
        self.observe_copy_losses();
        if !needs_translation
            && !self.has_recognition()
            && self.recording.is_none()
            && self.retained.is_none()
            && !self.history.enabled()
        {
            return None;
        }
        if let Some(tail) = self.speech.finish_before(original) {
            self.deliver(&tail);
        }
        let frame = self.speech.convert(original);
        self.deliver(&frame);
        Some(frame)
    }

    fn deliver(&mut self, frame: &audio::PcmFrame) {
        if frame.samples.is_empty() {
            return;
        }
        // Preserve originals before either writer or recognizer can reject a
        // frame. The store only copies into bounded RAM on this executor;
        // encrypted spill runs independently of the original audio transport.
        if let Some(retained) = self.retained
            && let Err(error) = retained.capture(frame, self.origin)
        {
            self.report_missing_originals(&format!(
                "Original audio retention failed; this frame cannot be guaranteed for recovery: {error}"
            ));
        }
        let lane = match self.origin {
            TranscriptOrigin::Microphone => RecordingLane::Microphone,
            TranscriptOrigin::Speaker => RecordingLane::Speaker,
        };
        self.history.push(lane, &frame.samples, frame.captured_at);
        if let Some(sender) = self.recording.as_ref() {
            let result = sender.try_send(AudioRecord {
                lane,
                samples: frame.samples.clone(),
                captured_at: frame.captured_at,
            });
            if result.is_err() {
                self.report_incomplete(if self.retained.is_some() {
                    "Audio recording needs recovery: its writer is unavailable or slow; accepted originals remain retained"
                } else {
                    "Audio recording interrupted: destination unavailable or slow; the file may be incomplete"
                });
                // A temporary full queue must not disable all subsequent
                // recording. The session owner can replay retained originals
                // after either congestion or a permanently closed writer.
            }
        }
        #[cfg(test)]
        if let Some(recognizer) = self.recognition {
            if recognizer.available() {
                let accepted = recognizer.submit(
                    audio::PcmFrame {
                        samples: frame.samples.clone(),
                        sample_rate: frame.sample_rate,
                        captured_at: frame.captured_at,
                    },
                    self.metrics,
                );
                if !accepted {
                    self.report_incomplete(if self.retained.is_some() {
                        "Transcription needs recovery: its input queue rejected audio; accepted originals remain retained"
                    } else {
                        "Transcription interrupted: its input queue rejected audio; the transcript may be incomplete"
                    });
                }
            } else {
                self.report_incomplete(if self.retained.is_some() {
                    "Transcription needs recovery: its recognizer is unavailable; accepted originals remain retained"
                } else {
                    "Transcription interrupted: its recognizer is unavailable; the transcript may be incomplete"
                });
            }
        }
    }

    fn drain(&mut self, captured: &mut mpsc::Receiver<audio::OriginalFrame>) {
        // Closing rejects new sends even if a malfunctioning device has not
        // released every task yet. Never await a producer or a model here.
        captured.close();
        self.observe_copy_losses();
        let count = captured.len();
        for _ in 0..count {
            let Ok(original) = captured.try_recv() else {
                break;
            };
            self.retain(&original, false);
        }
        if let Some(tail) = self.speech.finish() {
            self.deliver(&tail);
        }
        self.observe_copy_losses();
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum Processor {
    Translation,
}

/// Dropping a provider's sender can wake the receiver before its task result is
/// available. Preserve that result instead of reporting EOF as the root cause.
/// This runs on the processing executor; device workers keep their own runtime.
#[cfg(test)]
async fn translation_result_after_eof(
    jobs: &mut JoinSet<(Processor, Result<()>)>,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout(Duration::from_secs(2), async {
            match jobs.join_next().await {
                Some(completed) => match completed.context("Processing task ended unexpectedly")? {
                    (Processor::Translation, Err(error)) => Err(error),
                    (Processor::Translation, Ok(())) => bail!("The translator ended unexpectedly"),
                },
                None => bail!("Provider closed the audio channel without a completion result"),
            }
        }) => result.context("Provider closed the audio channel but did not finish within 2 seconds")?,
    }
}

#[cfg(test)]
fn spawn_processor<F>(
    jobs: &mut JoinSet<(Processor, Result<()>)>,
    handle: &tokio::runtime::Handle,
    kind: Processor,
    future: F,
) where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    jobs.spawn_on(
        async move {
            // Never expose an adapter panic payload through user-facing diagnostics.
            let result = std::panic::AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(anyhow!("Processing component failed internally")));
            (kind, result)
        },
        handle,
    );
}

async fn forward_processing(
    mut captured: mpsc::Receiver<audio::OriginalFrame>,
    processing: mpsc::Sender<audio::OriginalFrame>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
    drain_source: bool,
) -> Result<()> {
    let stopped_frame = loop {
        let frame = tokio::select! { biased; _ = cancel.cancelled() => break None, frame = captured.recv() => frame };
        let Some(frame) = frame else {
            return Err(anyhow!("Audio capture ended"));
        };
        if cancel.is_cancelled() {
            break Some(frame);
        }
        match processing.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(frame)) if cancel.is_cancelled() => {
                break Some(frame);
            }
            Err(_) => {
                stats
                    .processing_dropped_frames
                    .fetch_add(1, Ordering::Relaxed);
                stats.sidecar_dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
    };
    // Translated routes have this additional original-only queue. Preserve its
    // accepted frames for the session sidecar, with no network/playback work.
    // The route consumes sidecars concurrently during its bounded shutdown, so
    // a full destination can retain this tail without waiting for an AI model.
    if drain_source {
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            if let Some(frame) = stopped_frame {
                processing
                    .send(frame)
                    .await
                    .context("Original processing closed during shutdown")?;
            }
            while let Some(frame) = captured.recv().await {
                processing
                    .send(frame)
                    .await
                    .context("Original processing closed during shutdown")?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        return result.context("Original capture did not finish its bounded shutdown drain")?;
    }
    captured.close();
    let count = captured.len();
    if let Some(frame) = stopped_frame {
        processing
            .send(frame)
            .await
            .context("Original processing closed during shutdown")?;
    }
    for _ in 0..count {
        if let Ok(frame) = captured.try_recv() {
            processing
                .send(frame)
                .await
                .context("Original processing closed during shutdown")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original(captured_at: Instant) -> audio::OriginalFrame {
        audio::OriginalFrame {
            samples: vec![0.125; 160].into(),
            sample_rate: INPUT_RATE,
            channels: 1,
            captured_at,
        }
    }

    #[tokio::test]
    async fn capture_gap_keeps_subsequent_originals_available_to_consumers() {
        for origin in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
            let directory = tempfile::tempdir().unwrap();
            let store = crate::retention::SessionRetention::create_in(directory.path(), "fixture")
                .await
                .unwrap();
            let clock = Instant::now();
            let retained =
                retained::RetainedSession::new(store, clock, &tokio::runtime::Handle::current());
            let history =
                crate::history::HistoryBuffer::new(&crate::config::HistoryConfig::default());
            let metrics = RouteMetrics::default();
            let (sender, mut recorded) = mpsc::channel(2);
            let mut recording = Some(sender);
            let mut sidecar = OriginalSidecar {
                speech: audio::speech::SpeechTap::new(),
                history: &history,
                recording: &mut recording,
                recognition: None,
                retained: Some(&retained),
                origin,
                metrics: &metrics,
                copy_losses: 0,
                capture_losses: 0,
            };
            sidecar.retain(&original(clock + Duration::from_millis(10)), false);
            metrics
                .audio
                .capture_lost_frames
                .store(1, Ordering::Relaxed);
            sidecar.retain(&original(clock + Duration::from_millis(30)), false);
            assert!(retained.status().missing_audio);
            assert!(!retained.capture_requires_stop());
            assert!(metrics.snapshot().processing_error.is_some());
            for end in [10, 30] {
                assert_eq!(
                    recorded.try_recv().unwrap().captured_at,
                    clock + Duration::from_millis(end)
                );
            }
            retained.flush().await.unwrap();
            let mut replay = retained.snapshot().unwrap();
            for end in [10, 30] {
                assert_eq!(
                    replay.next().await.unwrap().unwrap().captured_at,
                    clock + Duration::from_millis(end)
                );
            }
            assert!(replay.next().await.unwrap().is_none());
            drop(replay);
            assert!(retained.complete().await.is_err());
        }
    }

    #[tokio::test]
    async fn shutdown_retains_speech_filter_tail_for_every_original_consumer() {
        for origin in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
            let directory = tempfile::tempdir().unwrap();
            let store = crate::retention::SessionRetention::create_in(directory.path(), "fixture")
                .await
                .unwrap();
            let clock = Instant::now() - Duration::from_millis(100);
            let retained =
                retained::RetainedSession::new(store, clock, &tokio::runtime::Handle::current());
            let history =
                crate::history::HistoryBuffer::new(&crate::config::HistoryConfig::default());
            let metrics = RouteMetrics::default();
            let (sender, mut recorded) = mpsc::channel(8);
            let mut recording = Some(sender);
            let mut sidecar = OriginalSidecar {
                speech: audio::speech::SpeechTap::new(),
                history: &history,
                recording: &mut recording,
                recognition: None,
                retained: Some(&retained),
                origin,
                metrics: &metrics,
                copy_losses: 0,
                capture_losses: 0,
            };
            for end in [10, 20, 30] {
                sidecar.retain(
                    &audio::OriginalFrame {
                        samples: vec![0.125; 480].into(),
                        sample_rate: 48_000,
                        channels: 1,
                        captured_at: clock + Duration::from_millis(end),
                    },
                    false,
                );
            }
            let (_capture, mut captured) = mpsc::channel(1);
            sidecar.drain(&mut captured);
            sidecar.drain(&mut captured);
            drop(sidecar);
            drop(recording);
            retained.close_capture();
            let mut reader = retained.reader(origin);
            let mut replayed = Vec::new();
            while let Some(frame) = reader.next().await.unwrap() {
                replayed.push(frame);
            }
            let mut records = Vec::new();
            while let Some(frame) = recorded.recv().await {
                records.push(frame);
            }
            assert_eq!(
                replayed
                    .iter()
                    .map(|frame| frame.samples.len())
                    .sum::<usize>(),
                480
            );
            assert_eq!(
                records
                    .iter()
                    .map(|frame| frame.samples.len())
                    .sum::<usize>(),
                480
            );
            assert_eq!(
                replayed.len(),
                4,
                "One delayed tail follows the three capture frames"
            );
            assert_eq!(
                replayed.last().unwrap().captured_at,
                clock + Duration::from_millis(30)
            );
            for (replayed, recorded) in replayed.iter().zip(records) {
                assert_eq!(replayed.samples, recorded.samples);
                assert_eq!(replayed.captured_at, recorded.captured_at);
            }
            let snapshot = history.snapshot(600, Instant::now());
            assert_eq!(
                snapshot
                    .frames
                    .iter()
                    .map(|frame| frame.sample_count())
                    .sum::<usize>(),
                480
            );
        }
    }

    #[tokio::test]
    async fn originals_remain_retained_when_recording_is_full_closed_or_unselected() {
        for origin in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
            let directory = tempfile::tempdir().unwrap();
            let store = crate::retention::SessionRetention::create_in(directory.path(), "fixture")
                .await
                .unwrap();
            let clock = Instant::now();
            let retained =
                retained::RetainedSession::new(store, clock, &tokio::runtime::Handle::current());
            let history = crate::history::HistoryBuffer::new(&crate::config::HistoryConfig {
                enabled: false,
                ..Default::default()
            });
            let metrics = RouteMetrics::default();
            let (sender, mut recorded) = mpsc::channel(1);
            let mut recording = Some(sender);
            let mut sidecar = OriginalSidecar {
                speech: audio::speech::SpeechTap::new(),
                history: &history,
                recording: &mut recording,
                recognition: None,
                retained: Some(&retained),
                origin,
                metrics: &metrics,
                copy_losses: 0,
                capture_losses: 0,
            };
            // The writer accepts the first frame, rejects the second, and then
            // recovers capacity. Its temporary backlog must not disable it.
            sidecar.retain(&original(clock + Duration::from_millis(10)), false);
            sidecar.retain(&original(clock + Duration::from_millis(20)), false);
            assert_eq!(
                recorded.try_recv().unwrap().captured_at,
                clock + Duration::from_millis(10)
            );
            sidecar.retain(&original(clock + Duration::from_millis(30)), false);
            assert_eq!(
                recorded.try_recv().unwrap().captured_at,
                clock + Duration::from_millis(30)
            );
            drop(recorded);
            sidecar.retain(&original(clock + Duration::from_millis(40)), false);
            assert!(sidecar.recording.is_some());
            // Retention itself remains sufficient to consume originals even
            // when no live consumer or rolling history remains selected.
            *sidecar.recording = None;
            sidecar.retain(&original(clock + Duration::from_millis(50)), false);
            assert_eq!(retained.status().retained_frames, 5);
            assert!(retained.status().error.unwrap().contains("recording"));
            assert!(!retained.status().completed);
            assert!(history.snapshot(600, clock).frames.is_empty());
            let lane = match origin {
                TranscriptOrigin::Microphone => RecordingLane::Microphone,
                TranscriptOrigin::Speaker => RecordingLane::Speaker,
            };
            let mut replay = retained.snapshot().unwrap();
            for end in [10, 20, 30, 40, 50] {
                let frame = replay.next().await.unwrap().unwrap();
                assert_eq!(frame.lane, lane);
                assert_eq!(frame.samples, vec![4096; 160]);
                assert_eq!(frame.captured_at, clock + Duration::from_millis(end));
            }
            assert!(replay.next().await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn empty_shutdown_tail_still_reports_originals_lost_before_retention() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::retention::SessionRetention::create_in(directory.path(), "fixture")
            .await
            .unwrap();
        let clock = Instant::now();
        let retained =
            retained::RetainedSession::new(store, clock, &tokio::runtime::Handle::current());
        let history = crate::history::HistoryBuffer::new(&crate::config::HistoryConfig::default());
        let metrics = RouteMetrics::default();
        let mut recording = None;
        let mut sidecar = OriginalSidecar {
            speech: audio::speech::SpeechTap::new(),
            history: &history,
            recording: &mut recording,
            recognition: None,
            retained: Some(&retained),
            origin: TranscriptOrigin::Speaker,
            metrics: &metrics,
            copy_losses: 0,
            capture_losses: 0,
        };
        sidecar.retain(&original(clock + Duration::from_millis(10)), false);
        let (capture, mut captured) = mpsc::channel(1);
        metrics.audio.dropped_frames.store(10, Ordering::Relaxed);
        sidecar.drain(&mut captured);
        assert!(
            retained.status().error.is_none(),
            "Playback drops are not original loss"
        );
        metrics
            .audio
            .sidecar_dropped_frames
            .store(1, Ordering::Relaxed);
        metrics
            .audio
            .capture_lost_frames
            .store(1, Ordering::Relaxed);
        sidecar.drain(&mut captured);
        assert!(capture.is_closed());
        assert_eq!(retained.status().retained_frames, 1);
        assert!(retained.status().missing_audio);
        assert!(
            retained
                .status()
                .error
                .unwrap()
                .contains("cannot be recovered"),
            "Later replay cannot recover a frame rejected before retention"
        );
        assert!(
            metrics
                .snapshot()
                .processing_error
                .unwrap()
                .contains("before session retention")
        );
        assert!(
            metrics
                .snapshot()
                .processing_error
                .unwrap()
                .contains("before session processing")
        );
    }

    #[tokio::test]
    async fn retention_failure_is_visible_without_blocking_original_consumer_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::retention::SessionRetention::create_in(directory.path(), "fixture")
            .await
            .unwrap();
        let clock = Instant::now();
        let retained =
            retained::RetainedSession::new(store, clock, &tokio::runtime::Handle::current());
        retained.close_capture();
        let history = crate::history::HistoryBuffer::new(&crate::config::HistoryConfig::default());
        let metrics = RouteMetrics::default();
        let (sender, mut recorded) = mpsc::channel(1);
        let mut recording = Some(sender);
        let mut sidecar = OriginalSidecar {
            speech: audio::speech::SpeechTap::new(),
            history: &history,
            recording: &mut recording,
            recognition: None,
            retained: Some(&retained),
            origin: TranscriptOrigin::Microphone,
            metrics: &metrics,
            copy_losses: 0,
            capture_losses: 0,
        };
        sidecar.retain(&original(clock + Duration::from_millis(10)), false);
        assert_eq!(recorded.try_recv().unwrap().samples, vec![4096; 160]);
        assert_eq!(retained.status().retained_frames, 0);
        assert!(retained.status().missing_audio);
        assert!(
            retained
                .status()
                .error
                .unwrap()
                .contains("retention failed")
        );
        assert!(
            metrics
                .snapshot()
                .processing_error
                .unwrap()
                .contains("retention failed")
        );
    }

    #[tokio::test]
    async fn original_tail_reaches_history_recording_and_final_transcription_for_both_sources() {
        use axum::{Json, Router, body::Bytes, routing::post};
        let requests = Arc::new(AtomicU64::new(0));
        let counted = requests.clone();
        let app = Router::new().route(
            "/inference",
            post(move |body: Bytes| {
                let counted = counted.clone();
                async move {
                    let start = body.windows(4).position(|bytes| bytes == b"RIFF").unwrap();
                    let mut wav =
                        hound::WavReader::new(std::io::Cursor::new(&body[start..])).unwrap();
                    let samples: Vec<i16> = wav
                        .samples()
                        .collect::<std::result::Result<_, _>>()
                        .unwrap();
                    assert_eq!(samples, vec![4096; 480]);
                    counted.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"text": "Original buffered speech."}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        for origin in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
            let mut config = AppConfig::default();
            config
                .transcription
                .providers
                .whisper
                .endpoint
                .clone_from(&endpoint);
            config.transcription.microphone_recognition.provider = "whisper".into();
            config.transcription.speaker_recognition.provider = "whisper".into();
            let clock = Instant::now() - Duration::from_millis(100);
            let metrics = Arc::new(RouteMetrics::default());
            let (text, mut transcripts) = mpsc::channel(8);
            let (sink, worker) = recognition::start(
                config,
                TranscriptSink {
                    sender: text,
                    origin,
                    retained: None,
                },
                metrics.clone(),
                clock,
            );
            let worker = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(worker));
            let history =
                crate::history::HistoryBuffer::new(&crate::config::HistoryConfig::default());
            let (recording, mut recorded) = mpsc::channel(8);
            let mut recording = Some(recording);
            let (capture, mut captured) = mpsc::channel(4);
            let mut originals = OriginalSidecar {
                speech: audio::speech::SpeechTap::new(),
                history: &history,
                recording: &mut recording,
                recognition: Some(&sink),
                retained: None,
                origin,
                metrics: &metrics,
                copy_losses: 0,
                capture_losses: 0,
            };
            // The first frame was already dequeued when cancellation was
            // noticed. The other two remain in the bounded sidecar queue.
            originals.retain(&original(clock + Duration::from_millis(10)), false);
            for end in [20, 30] {
                capture
                    .send(original(clock + Duration::from_millis(end)))
                    .await
                    .unwrap();
            }
            originals.drain(&mut captured);
            assert!(
                capture.is_closed(),
                "Draining never waits for this producer to exit"
            );
            drop(originals);
            drop(recording);
            drop(sink);
            tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let lane = match origin {
                TranscriptOrigin::Microphone => RecordingLane::Microphone,
                TranscriptOrigin::Speaker => RecordingLane::Speaker,
            };
            for end in [10, 20, 30] {
                let frame = recorded.try_recv().unwrap();
                assert_eq!(frame.lane, lane);
                assert_eq!(frame.samples, vec![4096; 160]);
                assert_eq!(frame.captured_at, clock + Duration::from_millis(end));
            }
            assert!(recorded.try_recv().is_err());
            let snapshot = history.snapshot(600, Instant::now());
            assert_eq!(snapshot.frames.len(), 3);
            let mut reader = crate::history::HistoryReader::default();
            for frame in &snapshot.frames {
                assert_eq!(frame.lane, lane);
                assert_eq!(
                    frame.read_samples(&mut reader).await.unwrap().as_slice(),
                    &[4096; 160]
                );
            }
            let TranscriptRecord::Routed {
                origin: source,
                record,
            } = transcripts.try_recv().unwrap()
            else {
                panic!("expected routed transcript");
            };
            assert_eq!(source, origin);
            assert!(
                matches!(*record, TranscriptRecord::Text { input: true, text, metadata, .. }
                if text == "Original buffered speech." && metadata.start_ms == Some(0) && metadata.end_ms == Some(30))
            );
            assert_eq!(
                metrics
                    .audio
                    .processing_dropped_frames
                    .load(Ordering::Relaxed),
                0
            );
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        drop(server);
    }

    #[tokio::test]
    async fn production_processing_shutdown_preserves_frames_delivered_after_cancel() {
        let clock = Instant::now();
        let (capture, captured) = mpsc::channel(2);
        let (sidecar, mut received) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let stats = Arc::new(AudioStats::default());
        let worker = tokio::spawn(forward_processing(
            captured,
            sidecar,
            cancel,
            stats.clone(),
            true,
        ));
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        capture.send(original(clock)).await.unwrap();
        drop(capture);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(received.recv().await.unwrap().captured_at, clock);
        assert!(received.recv().await.is_none());
        assert_eq!(stats.capture_lost_frames.load(Ordering::Relaxed), 0);
        assert_eq!(stats.sidecar_dropped_frames.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn stopped_capture_preserves_its_bounded_tail_when_the_processing_queue_is_full() {
        for capacity in [1, 4] {
            let clock = Instant::now();
            let (capture, captured) = mpsc::channel(4);
            for index in capacity..capacity + 3 {
                capture
                    .send(original(clock + Duration::from_millis(index as u64)))
                    .await
                    .unwrap();
            }
            let (sidecar, mut received) = mpsc::channel(capacity);
            for index in 0..capacity {
                sidecar
                    .send(original(clock + Duration::from_millis(index as u64)))
                    .await
                    .unwrap();
            }
            let cancel = CancellationToken::new();
            cancel.cancel();
            let stats = Arc::new(AudioStats::default());
            let forwarding = tokio::spawn(forward_processing(
                captured,
                sidecar,
                cancel,
                stats.clone(),
                false,
            ));
            tokio::time::timeout(Duration::from_secs(5), async {
                while !capture.is_closed() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(
                !forwarding.is_finished(),
                "A full queue must retain the accepted tail"
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                for index in 0..capacity + 3 {
                    let frame = received.recv().await.unwrap();
                    assert_eq!(frame.samples.as_ref(), &[0.125; 160]);
                    assert_eq!(
                        frame.captured_at,
                        clock + Duration::from_millis(index as u64)
                    );
                }
                forwarding.await.unwrap().unwrap();
                assert!(received.recv().await.is_none());
            })
            .await
            .unwrap();
            assert_eq!(stats.sidecar_dropped_frames.load(Ordering::Relaxed), 0);
            assert_eq!(stats.processing_dropped_frames.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn provider_eof_preserves_delayed_task_error_and_buffered_events() {
        let mut jobs = JoinSet::new();
        let (events, mut received) = mpsc::channel(2);
        let (finish, finished) = tokio::sync::oneshot::channel();
        spawn_processor(
            &mut jobs,
            &tokio::runtime::Handle::current(),
            Processor::Translation,
            async move {
                events.send(ProviderEvent::Connected).await?;
                events.send(ProviderEvent::TurnComplete).await?;
                drop(events);
                finished.await?;
                bail!("synthetic translator HTTP 503")
            },
        );
        assert_eq!(received.recv().await, Some(ProviderEvent::Connected));
        assert_eq!(received.recv().await, Some(ProviderEvent::TurnComplete));
        assert_eq!(received.recv().await, None);
        let cancel = CancellationToken::new();
        let completion = translation_result_after_eof(&mut jobs, &cancel);
        tokio::pin!(completion);
        assert!(completion.as_mut().now_or_never().is_none());
        finish.send(()).unwrap();
        let error = completion.await.unwrap_err();
        assert!(error.to_string().contains("synthetic translator HTTP 503"));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_eof_wait_is_bounded_and_cancellable() {
        for stopping in [false, true] {
            let mut jobs = JoinSet::new();
            spawn_processor(
                &mut jobs,
                &tokio::runtime::Handle::current(),
                Processor::Translation,
                std::future::pending(),
            );
            let cancel = CancellationToken::new();
            let started = tokio::time::Instant::now();
            let completion = translation_result_after_eof(&mut jobs, &cancel);
            tokio::pin!(completion);
            assert!(completion.as_mut().now_or_never().is_none());
            if stopping {
                cancel.cancel();
                completion.await.unwrap();
                assert_eq!(started.elapsed(), Duration::ZERO);
            } else {
                let error = completion.await.unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("did not finish within 2 seconds")
                );
                assert_eq!(started.elapsed(), Duration::from_secs(2));
            }
        }
    }

    #[tokio::test]
    async fn panicking_translator_returns_a_sanitized_processing_failure() {
        let mut jobs = JoinSet::new();
        spawn_processor(
            &mut jobs,
            &tokio::runtime::Handle::current(),
            Processor::Translation,
            async {
                panic!("fault injected in speech adapter");
                #[allow(unreachable_code)]
                Ok(())
            },
        );
        let (kind, result) = jobs
            .join_next()
            .await
            .unwrap()
            .expect("panic is contained in the adapter");
        assert!(matches!(kind, Processor::Translation));
        assert!(result.unwrap_err().to_string().contains("Processing"));
    }
}
