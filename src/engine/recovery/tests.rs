use super::*;

async fn fixture() -> (tempfile::TempDir, Controller, Archive) {
    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.files.base_path = directory.path().to_str().unwrap().into();
    config.recording.enabled = true;
    config.recording.mix = crate::config::RecordingMixConfig::transparent();
    config.transcription.enabled = false;
    config.microphone.enabled = false;
    config.speaker.enabled = false;
    config.microphone.capture_device = "fixture-physical-microphone".into();
    config.microphone.playback_device = "fixture-virtual-microphone".into();
    config.speaker.capture_device = "fixture-virtual-speaker".into();
    config.speaker.playback_device = "fixture-physical-speaker".into();
    let controller = Controller::new(config.clone(), directory.path().join("babel.toml")).unwrap();
    let session =
        crate::session::SessionIdentity::new_with_language(Some("Recovery fixture"), "en").unwrap();
    let store = crate::retention::SessionRetention::create_in(directory.path(), "recovery-fixture")
        .await
        .unwrap();
    let origin = Instant::now() - Duration::from_secs(1);
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for (lane, value) in [
        (TranscriptOrigin::Microphone, 1000),
        (TranscriptOrigin::Speaker, 3000),
    ] {
        originals
            .capture(
                &audio::PcmFrame {
                    samples: vec![value; 320],
                    sample_rate: INPUT_RATE,
                    captured_at: origin + Duration::from_millis(20),
                },
                lane,
            )
            .unwrap();
    }
    let archive = Archive::new(
        session,
        config,
        origin,
        originals,
        crate::history::HistorySnapshot {
            origin,
            frames: Vec::new(),
            included_secs: 0.0,
        },
    );
    (directory, controller, archive)
}

async fn wait_for_cleanup(archive: &Archive) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !archive.originals.status().completed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Committed output releases encrypted retention asynchronously");
}

async fn original_samples(archive: &Archive) -> Vec<(RecordingLane, Vec<i16>)> {
    let mut replay = archive.originals.snapshot().unwrap();
    let mut values = Vec::new();
    while let Some(frame) = replay.next().await.unwrap() {
        values.push((frame.lane, frame.samples));
    }
    values
}

#[tokio::test]
async fn failed_recording_worker_keeps_both_originals_in_controller_ownership() {
    let (_directory, controller, archive) = fixture().await;
    let files = create_session_files(&archive.config, &archive.session, archive.origin)
        .await
        .unwrap();
    let (sender, received) = mpsc::channel(1);
    sender
        .send(AudioRecord {
            lane: RecordingLane::Microphone,
            samples: vec![],
            captured_at: archive.origin,
        })
        .await
        .unwrap();
    drop(sender);
    let failure = files.audio.unwrap().run(received).await.unwrap_err();
    let mut status = stopped_status();
    status.last_error = Some(format!("Recording writer failed: {failure:#}"));
    finish_archive(
        &mut *controller.state.lock().await,
        Some(archive.clone()),
        &mut status,
    )
    .await;
    assert_eq!(controller.retained_sessions().await.len(), 1);
    assert_eq!(archive.originals.status().frames, 2);
    assert!(archive.originals.status().capture_closed);
    assert!(status.last_error.unwrap().contains("retained for recovery"));
    let retained = original_samples(&archive).await;
    assert_eq!(
        retained,
        vec![
            (RecordingLane::Microphone, vec![1000; 320]),
            (RecordingLane::Speaker, vec![3000; 320])
        ]
    );
}

#[tokio::test]
async fn a_successful_session_acknowledges_originals_after_final_output_sync() {
    let (_directory, controller, archive) = fixture().await;
    archive.originals.flush().await.unwrap();
    assert_eq!(archive.originals.status().encrypted_frames, 2);
    let files = create_session_files(&archive.config, &archive.session, archive.origin)
        .await
        .unwrap();
    let (_usage, usage) = watch::channel(audio::activity::EndpointUse::default());
    replay(&archive, &archive.config, files, usage)
        .await
        .unwrap();
    archive.outputs_committed.store(true, Ordering::Release);
    let mut status = stopped_status();
    finish_archive(
        &mut *controller.state.lock().await,
        Some(archive.clone()),
        &mut status,
    )
    .await;
    assert!(controller.retained_sessions().await.is_empty());
    wait_for_cleanup(&archive).await;
    assert_eq!(archive.originals.status().frames, 0);
    assert!(archive.originals.status().completed);
    assert!(archive.originals.snapshot().is_err());
}

#[tokio::test]
async fn recording_recovery_merges_original_lanes_and_preserves_the_earlier_partial_file() {
    let (directory, controller, archive) = fixture().await;
    let original_stem = archive
        .session
        .file_stem(&archive.config.files.name_pattern)
        .unwrap();
    let recording_folder = directory.path().join(&archive.config.recording.directory);
    std::fs::create_dir_all(&recording_folder).unwrap();
    let original_path = recording_folder.join(format!("{original_stem}.wav"));
    let earlier_bytes = b"Previously saved partial session output";
    std::fs::write(&original_path, earlier_bytes).unwrap();
    archive
        .originals
        .mark_incomplete("Recording writer stopped before EOF");
    archive.originals.close_capture();
    archive.originals.flush().await.unwrap();
    controller.state.lock().await.retained.push(archive.clone());
    controller
        .recover_retained_session(&archive.session.id)
        .await
        .unwrap();
    assert!(controller.retained_sessions().await.is_empty());
    assert_eq!(std::fs::read(&original_path).unwrap(), earlier_bytes);
    let recovered: Vec<_> = std::fs::read_dir(recording_folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path != &original_path)
        .collect();
    assert_eq!(recovered.len(), 1);
    let mut wave = hound::WavReader::open(&recovered[0]).unwrap();
    assert_eq!(wave.spec().sample_rate, 16_000);
    assert_eq!(wave.spec().channels, 1);
    assert_eq!(
        wave.samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap(),
        vec![2000; 320]
    );
    wait_for_cleanup(&archive).await;
    assert!(archive.originals.status().completed);
}

#[tokio::test]
async fn failed_recovery_preserves_encrypted_originals_and_their_ram_key_for_retry() {
    let (directory, controller, mut archive) = fixture().await;
    archive.originals.close_capture();
    archive.originals.flush().await.unwrap();
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, b"Fixture storage is unavailable").unwrap();
    archive.config.recording.directory = blocked.to_str().unwrap().into();
    controller.state.lock().await.retained.push(archive.clone());
    assert!(
        controller
            .recover_retained_session(&archive.session.id)
            .await
            .is_err()
    );
    let pending = controller.retained_sessions().await;
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].recovering);
    assert_eq!(pending[0].frames, 2);
    assert!(
        pending[0]
            .error
            .as_ref()
            .unwrap()
            .contains("Recovery failed")
    );
    assert_eq!(archive.originals.status().encrypted_frames, 2);
    assert_eq!(
        original_samples(&archive).await,
        vec![
            (RecordingLane::Microphone, vec![1000; 320]),
            (RecordingLane::Speaker, vec![3000; 320])
        ]
    );
}

#[tokio::test]
async fn duplicate_recovery_is_rejected_without_releasing_pending_originals() {
    let (_directory, controller, archive) = fixture().await;
    archive.originals.close_capture();
    archive.recovering.store(true, Ordering::Release);
    controller.state.lock().await.retained.push(archive.clone());
    let error = controller
        .recover_retained_session(&archive.session.id)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("already being recovered"));
    assert_eq!(archive.originals.status().frames, 2);
    assert_eq!(controller.retained_sessions().await.len(), 1);
    assert!(
        archive.recovering.load(Ordering::Acquire),
        "The existing recovery retains its exclusive ownership"
    );
}

#[tokio::test]
async fn recovered_available_audio_cannot_hide_an_upstream_original_gap() {
    let (_directory, controller, archive) = fixture().await;
    archive
        .originals
        .mark_unrecoverable("Capture lost an original frame before retention");
    archive.originals.close_capture();
    controller.state.lock().await.retained.push(archive.clone());
    assert!(
        controller
            .recover_retained_session(&archive.session.id)
            .await
            .is_err()
    );
    assert_eq!(controller.retained_sessions().await.len(), 1);
    assert_eq!(archive.originals.status().frames, 2);
    assert!(archive.originals.status().missing_audio);
    assert!(!archive.originals.status().completed);
    assert_eq!(original_samples(&archive).await.len(), 2);
}

#[tokio::test]
async fn failed_history_only_session_preserves_shared_originals_and_recovers_both_sources() {
    let (directory, controller, mut archive) = fixture().await;
    let history = crate::history::HistoryBuffer::new(&archive.config.history);
    let captured_at = Instant::now();
    history.push(RecordingLane::Microphone, &[1000; 320], captured_at);
    history.push(RecordingLane::Speaker, &[3000; 320], captured_at);
    history.flush().await;
    assert_eq!(history.status(captured_at).buffered_bytes, 0);
    let prefix = history.snapshot(1, captured_at);
    assert_eq!(prefix.frames.len(), 2);
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "history-only-fixture")
            .await
            .unwrap();
    archive.originals =
        retained::RetainedSession::new(store, prefix.origin, &tokio::runtime::Handle::current());
    archive.origin = prefix.origin;
    archive.history = prefix.clone();
    for (source, archived) in prefix.frames.iter().zip(&archive.history.frames) {
        assert!(
            Arc::ptr_eq(&source.samples, &archived.samples),
            "Retaining history shares PCM instead of copying the entire prefix"
        );
    }
    assert_eq!(archive.originals.status().frames, 0);
    let mut status = stopped_status();
    status.last_error = Some("History processing ended before completion".into());
    finish_archive(
        &mut *controller.state.lock().await,
        Some(archive.clone()),
        &mut status,
    )
    .await;
    let pending = controller.retained_sessions().await;
    assert_eq!(
        pending.len(),
        1,
        "No live frames does not mean there is no historical audio to recover"
    );
    assert_eq!(pending[0].frames, 2);
    controller
        .recover_retained_session(&archive.session.id)
        .await
        .unwrap();
    assert!(controller.retained_sessions().await.is_empty());
    let files: Vec<_> =
        std::fs::read_dir(directory.path().join(&archive.config.recording.directory))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
    assert_eq!(files.len(), 1);
    let mut wave = hound::WavReader::open(&files[0]).unwrap();
    assert_eq!(
        wave.samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap(),
        vec![2000; 320]
    );
    wait_for_cleanup(&archive).await;
    assert!(archive.originals.status().completed);
}

#[derive(Default)]
struct WindowRecognizer {
    calls: Arc<StdMutex<Vec<usize>>>,
}

#[async_trait::async_trait]
impl provider::SpeechProvider for WindowRecognizer {
    fn id(&self) -> &'static str {
        "finite-original-window-fixture"
    }

    async fn run(
        &self,
        _settings: SessionConfig,
        _audio: mpsc::Receiver<Vec<i16>>,
        _events: mpsc::Sender<ProviderEvent>,
        _cancel: CancellationToken,
    ) -> Result<()> {
        bail!("Recovery must use finite original transcription");
    }

    async fn run_history(
        &self,
        _settings: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let mut samples = 0;
        while let Some(frame) = audio.recv().await {
            ensure!(!cancel.is_cancelled(), "Fixture recognizer cancelled");
            ensure!(
                frame.len() <= INPUT_RATE as usize,
                "A model message exceeded one second"
            );
            ensure!(
                frame.iter().all(|sample| matches!(*sample, 0 | 7)),
                "Recovery changed original PCM"
            );
            samples += frame.len();
        }
        ensure!(
            samples > 0 && samples <= 31 * INPUT_RATE as usize,
            "A recovery request exceeded its bounded window"
        );
        let index = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(samples);
            calls.len()
        };
        events
            .send(ProviderEvent::Transcript {
                input: false,
                text: "Generated speech is not an original transcript".into(),
                metadata: Default::default(),
            })
            .await?;
        events
            .send(ProviderEvent::Transcript {
                input: true,
                text: format!("Original window {index}."),
                metadata: provider::TranscriptMetadata {
                    start_ms: Some(100),
                    end_ms: Some(samples as u64 / 16),
                    alignment_ms: Some(200),
                    speaker: Some("original-speaker".into()),
                },
            })
            .await?;
        events.send(ProviderEvent::TurnComplete).await?;
        Ok(())
    }
}

async fn recognize_fixture_windows(
    samples: Vec<i16>,
    chunk_samples: usize,
    origin: TranscriptOrigin,
) -> (Vec<usize>, Vec<TranscriptRecord>) {
    let provider = Arc::new(WindowRecognizer::default());
    let calls = provider.calls.clone();
    let config = AppConfig::default();
    let settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    let (input, received) = mpsc::channel(8);
    let (text, mut records) = mpsc::channel(8);
    let supply = async move {
        for chunk in samples.chunks(chunk_samples) {
            input.send(chunk.to_vec()).await.unwrap();
        }
    };
    let consume = async move {
        let mut output = Vec::new();
        while let Some(record) = records.recv().await {
            output.push(record);
        }
        output
    };
    let (_, result, output) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            supply,
            transcribe(
                provider,
                settings,
                received,
                text,
                origin,
                CancellationToken::new()
            ),
            consume
        )
    })
    .await
    .expect("Finite original transcription must finish when input closes");
    result.unwrap();
    let counts = calls.lock().unwrap().clone();
    (counts, output)
}

#[tokio::test]
async fn long_original_transcription_uses_bounded_windows_with_continuous_metadata() {
    for source in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
        // Uneven chunks cross the nominal 30-second boundary, so offsets must
        // follow actual submitted PCM rather than assuming fixed-size windows.
        let (calls, records) =
            recognize_fixture_windows(vec![7; 61 * 16_000], 12_799, source).await;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls.iter().sum::<usize>(), 61 * 16_000);
        assert!(calls.iter().all(|samples| *samples <= 31 * 16_000));
        let mut offset_samples = 0;
        let mut utterances = 0;
        let mut boundaries = 0;
        for record in records {
            let TranscriptRecord::Routed { origin, record } = record else {
                panic!("Missing original source label");
            };
            assert_eq!(origin, source);
            match *record {
                TranscriptRecord::Text {
                    input,
                    text,
                    metadata,
                    ..
                } => {
                    assert!(input);
                    assert_eq!(text, format!("Original window {}.", utterances + 1));
                    let offset_ms = offset_samples as u64 / 16;
                    assert_eq!(metadata.start_ms, Some(offset_ms + 100));
                    assert_eq!(
                        metadata.end_ms,
                        Some(offset_ms + calls[utterances] as u64 / 16)
                    );
                    assert_eq!(metadata.alignment_ms, Some(offset_ms + 200));
                    assert_eq!(metadata.speaker.as_deref(), Some("original-speaker"));
                    offset_samples += calls[utterances];
                    utterances += 1;
                }
                TranscriptRecord::TurnComplete => boundaries += 1,
                _ => panic!("Unexpected record from finite recognition"),
            }
        }
        assert_eq!(utterances, 3);
        assert_eq!(boundaries, 3);
    }
}

#[tokio::test]
async fn empty_and_digital_silence_sources_do_not_open_a_recognizer() {
    for source in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
        for seconds in [0, 61] {
            let (calls, records) =
                recognize_fixture_windows(vec![0; seconds * 16_000], 16_000, source).await;
            assert!(calls.is_empty());
            assert!(records.is_empty());
        }
    }
}

#[tokio::test]
async fn skipped_silent_windows_still_advance_original_transcript_offsets() {
    let mut samples = vec![0; 30 * 16_000];
    samples.extend(vec![7; 16_000]);
    let (calls, records) =
        recognize_fixture_windows(samples, 16_000, TranscriptOrigin::Speaker).await;
    assert_eq!(calls, vec![16_000]);
    let TranscriptRecord::Routed { origin, record } = &records[0] else {
        panic!("Missing source label");
    };
    assert_eq!(*origin, TranscriptOrigin::Speaker);
    let TranscriptRecord::Text { metadata, text, .. } = record.as_ref() else {
        panic!("Missing original words");
    };
    assert_eq!(text, "Original window 1.");
    assert_eq!(metadata.start_ms, Some(30_100));
    assert_eq!(metadata.end_ms, Some(31_000));
    assert_eq!(metadata.alignment_ms, Some(30_200));
}

#[tokio::test]
async fn translation_failure_does_not_retain_already_committed_original_outputs() {
    let (_directory, controller, archive) = fixture().await;
    let files = create_session_files(&archive.config, &archive.session, archive.origin)
        .await
        .unwrap();
    let (_usage, usage) = watch::channel(audio::activity::EndpointUse::default());
    replay(&archive, &archive.config, files, usage)
        .await
        .unwrap();
    archive.outputs_committed.store(true, Ordering::Release);
    let mut status = stopped_status();
    status.last_error = Some("Translation provider disconnected".into());
    status.speaker.processing_error = Some("Translation could not continue".into());
    finish_archive(
        &mut *controller.state.lock().await,
        Some(archive.clone()),
        &mut status,
    )
    .await;
    assert!(
        controller.retained_sessions().await.is_empty(),
        "Committed original files do not need a second paid transcription"
    );
    assert_eq!(
        status.last_error.as_deref(),
        Some("Translation provider disconnected")
    );
    wait_for_cleanup(&archive).await;
    assert!(archive.originals.status().completed);
}
