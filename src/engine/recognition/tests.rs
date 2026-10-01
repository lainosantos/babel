use super::*;
use crate::{audio::activity::EndpointUse, provider::TranscriptMetadata};
use async_trait::async_trait;

#[derive(Default)]
struct RetainedRecognizer {
    inputs: StdMutex<Vec<(String, Vec<i16>)>>,
    unavailable: bool,
    retry_started: Notify,
    finish_retry: Notify,
}

#[async_trait]
impl SpeechProvider for RetainedRecognizer {
    fn id(&self) -> &'static str {
        "gemini"
    }
    async fn run_history(
        &self,
        config: SessionConfig,
        audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        self.run(config, audio, events, cancel).await
    }
    async fn run(
        &self,
        config: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        _: CancellationToken,
    ) -> Result<()> {
        assert!(config.target_language.is_empty());
        assert!(!config.output_transcription);
        assert_eq!(config.max_reconnect_attempts, 0);
        let mut pcm = Vec::new();
        while let Some(samples) = audio.recv().await {
            pcm.extend(samples);
        }
        let first = {
            let mut inputs = self.inputs.lock().unwrap();
            let first = !inputs
                .iter()
                .any(|(language, _)| *language == config.source_language);
            inputs.push((config.source_language.clone(), pcm.clone()));
            first
        };
        if self.unavailable || config.source_language == "pt-BR" && first {
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Unconfirmed partial result".into(),
                    metadata: Default::default(),
                })
                .await?;
            bail!("Synthetic original recognizer unavailable");
        }
        if config.source_language == "pt-BR" {
            self.retry_started.notify_one();
            self.finish_retry.notified().await;
        }
        let value = if config.source_language == "pt-BR" {
            42
        } else {
            -42
        };
        assert_eq!(pcm, vec![value; 16000]);
        events
            .send(ProviderEvent::Transcript {
                input: false,
                text: "Generated speech must not enter the TXT".into(),
                metadata: Default::default(),
            })
            .await?;
        events
            .send(ProviderEvent::Audio {
                samples: vec![999; 240],
                sample_rate: 24000,
            })
            .await?;
        events
            .send(ProviderEvent::Transcript {
                input: true,
                text: if value == 42 {
                    "Minha fala original."
                } else {
                    "Original incoming speech."
                }
                .into(),
                metadata: TranscriptMetadata {
                    alignment_ms: Some(0),
                    ..Default::default()
                },
            })
            .await?;
        events.send(ProviderEvent::TurnComplete).await?;
        Ok(())
    }
}

#[tokio::test]
async fn retained_live_transcription_recovers_exact_pcm_and_keeps_both_originals_separate() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "separate-originals")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    originals
        .capture(
            &frame(origin, 1000, 16000, 42),
            TranscriptOrigin::Microphone,
        )
        .unwrap();
    originals
        .capture(&frame(origin, 1000, 16000, -42), TranscriptOrigin::Speaker)
        .unwrap();
    originals.close_capture();
    let provider = Arc::new(RetainedRecognizer::default());
    let (text, mut records) = mpsc::channel(16);
    let mut config = AppConfig::default();
    config.transcription.microphone_recognition.provider = "gemini".into();
    config.transcription.microphone_recognition.language = "pt-BR".into();
    config.transcription.speaker_recognition.provider = "gemini".into();
    config.transcription.speaker_recognition.language = "en-US".into();
    let metrics = [
        Arc::new(RouteMetrics::default()),
        Arc::new(RouteMetrics::default()),
    ];
    let mut tasks = Vec::new();
    for (index, source, route) in [
        (
            0,
            TranscriptOrigin::Microphone,
            &config.transcription.microphone_recognition,
        ),
        (
            1,
            TranscriptOrigin::Speaker,
            &config.transcription.speaker_recognition,
        ),
    ] {
        tasks.push(tokio::spawn(run_retained(
            provider.clone(),
            provider::stt::session_config(route, &config.transcription.providers).unwrap(),
            TranscriptSink {
                retained: Some(originals.clone()),
                sender: text.clone(),
                origin: source,
            },
            metrics[index].clone(),
            origin,
            originals.clone(),
        )));
    }
    drop(text);
    tokio::time::timeout(Duration::from_secs(5), provider.retry_started.notified())
        .await
        .unwrap();
    assert!(
        metrics[0]
            .snapshot()
            .processing_error
            .unwrap()
            .contains("Synthetic original recognizer unavailable")
    );
    assert!(
        metrics[0]
            .snapshot()
            .recovering
            .contains(&"transcription".to_owned())
    );
    assert!(metrics[1].snapshot().processing_error.is_none());
    provider.finish_retry.notify_one();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let mut texts = Vec::new();
    while let Some(record) = records.recv().await {
        if let TranscriptRecord::Routed { origin, record } = record
            && let TranscriptRecord::Text { text, metadata, .. } = *record
        {
            assert_eq!(metadata.alignment_ms, Some(0));
            texts.push((origin, text));
        }
    }
    assert_eq!(texts.len(), 2);
    assert!(texts.contains(&(TranscriptOrigin::Microphone, "Minha fala original.".into())));
    assert!(texts.contains(&(
        TranscriptOrigin::Speaker,
        "Original incoming speech.".into()
    )));
    let inputs = provider.inputs.lock().unwrap();
    assert_eq!(
        inputs
            .iter()
            .filter(|(language, pcm)| language == "pt-BR" && *pcm == vec![42; 16000])
            .count(),
        2
    );
    assert_eq!(
        inputs
            .iter()
            .filter(|(language, pcm)| language == "en-US" && *pcm == vec![-42; 16000])
            .count(),
        1
    );
    assert!(
        metrics
            .iter()
            .all(|metrics| metrics.snapshot().processing_error.is_none())
    );
    assert_eq!(originals.status().frames, 2);
}

#[tokio::test]
async fn retained_transcription_failure_stops_retrying_without_acknowledging_or_releasing_originals()
 {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "bounded-originals")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    originals
        .capture(
            &frame(origin, 1000, 16000, 42),
            TranscriptOrigin::Microphone,
        )
        .unwrap();
    originals.close_capture();
    let processor = Arc::new(RetainedRecognizer {
        unavailable: true,
        ..Default::default()
    });
    let config = AppConfig::default();
    let mut settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    settings.max_reconnect_attempts = 3;
    let (text, mut records) = mpsc::channel(1);
    let metrics = Arc::new(RouteMetrics::default());
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        run_retained(
            processor.clone(),
            settings,
            TranscriptSink {
                retained: Some(originals.clone()),
                sender: text,
                origin: TranscriptOrigin::Microphone,
            },
            metrics.clone(),
            origin,
            originals.clone(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("Automatic recovery paused"));
    assert_eq!(processor.inputs.lock().unwrap().len(), 4);
    assert!(records.recv().await.is_none());
    assert_eq!(originals.status().frames, 1);
    assert!(!originals.status().completed);
    assert!(metrics.snapshot().processing_error.is_some());
}

#[tokio::test]
async fn active_transcription_keeps_retrying_and_stop_wakes_its_cooldown_without_another_request() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "active-original-retry")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    originals
        .capture(
            &frame(origin, 1000, 16000, 42),
            TranscriptOrigin::Microphone,
        )
        .unwrap();
    let processor = Arc::new(RetainedRecognizer {
        unavailable: true,
        ..Default::default()
    });
    let config = AppConfig::default();
    let mut settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    settings.max_reconnect_attempts = 3;
    let (text, _records) = mpsc::channel(1);
    let metrics = Arc::new(RouteMetrics::default());
    let task = tokio::spawn(run_retained(
        processor.clone(),
        settings,
        TranscriptSink {
            retained: Some(originals.clone()),
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        metrics.clone(),
        origin,
        originals.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while metrics.reconnects.load(Ordering::Relaxed) < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    originals.close_capture();
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("Automatic recovery paused"));
    assert_eq!(processor.inputs.lock().unwrap().len(), 4);
    assert_eq!(originals.status().frames, 1);
}

struct Recognizer {
    first: i16,
    finalize_at_eof: bool,
    received: Arc<AtomicU64>,
    setup: Option<Arc<Notify>>,
}

#[async_trait]
impl SpeechProvider for Recognizer {
    fn id(&self) -> &'static str {
        "synthetic-original-stt"
    }
    async fn run(
        &self,
        _config: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        if let Some(setup) = &self.setup {
            setup.notified().await;
        }
        ensure!(!cancel.is_cancelled(), "Recognizer cancelled during setup");
        events.send(ProviderEvent::Connected).await?;
        let mut count = 0u64;
        while let Some(samples) = audio.recv().await {
            ensure!(
                samples.iter().all(|value| *value == self.first),
                "STT received translated or another source's PCM"
            );
            count += samples.len() as u64;
            self.received.store(count, Ordering::Relaxed);
            if !self.finalize_at_eof {
                self.final_text(&events, count).await?;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        ensure!(
            !cancel.is_cancelled(),
            "Recognizer cancelled before final result"
        );
        if self.finalize_at_eof {
            self.final_text(&events, count).await?;
        }
        Ok(())
    }
}
impl Recognizer {
    async fn final_text(&self, events: &mpsc::Sender<ProviderEvent>, samples: u64) -> Result<()> {
        // A faulty recognizer's generated output is still never saved.
        events
            .send(ProviderEvent::Transcript {
                input: false,
                text: "generated translation".into(),
                metadata: TranscriptMetadata::default(),
            })
            .await?;
        events
            .send(ProviderEvent::Audio {
                samples: vec![999],
                sample_rate: 24000,
            })
            .await?;
        events
            .send(ProviderEvent::Transcript {
                input: true,
                text: format!("Original source {}.", self.first),
                metadata: TranscriptMetadata {
                    start_ms: Some(0),
                    end_ms: Some(samples * 1000 / u64::from(INPUT_RATE)),
                    ..Default::default()
                },
            })
            .await?;
        events.send(ProviderEvent::TurnComplete).await?;
        Ok(())
    }
}

fn frame(origin: Instant, end_ms: u64, samples: usize, value: i16) -> PcmFrame {
    PcmFrame {
        captured_at: origin + Duration::from_millis(end_ms),
        sample_rate: INPUT_RATE,
        samples: vec![value; samples],
    }
}

fn launch(
    provider: Arc<dyn SpeechProvider>,
    transcript: TranscriptSink,
    metrics: Arc<RouteMetrics>,
    origin: Instant,
) -> (Sink, JoinHandle<Result<()>>) {
    let (sender, mut received) = mpsc::channel(QUEUED_FRAMES);
    let sink = Sink {
        sender,
        budget: Arc::new(Semaphore::new(QUEUED_SAMPLES)),
        connected: Arc::new(AtomicBool::new(false)),
        losses: Arc::new(AtomicU64::new(0)),
    };
    let connected = sink.connected.clone();
    let losses = sink.losses.clone();
    let config = AppConfig::default();
    let settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    let task = tokio::spawn(async move {
        let Some(first) = received.recv().await else {
            return Ok(());
        };
        run(
            provider,
            settings,
            received,
            first,
            &Some(transcript),
            &metrics,
            origin,
            &connected,
            &losses,
        )
        .await
    });
    (sink, task)
}

#[tokio::test]
async fn speaker_pause_closes_device_before_delayed_setup_and_preserves_original_text() {
    let origin = Instant::now();
    let metrics = Arc::new(RouteMetrics::default());
    let (text, mut records) = mpsc::channel(16);
    let received = Arc::new(AtomicU64::new(0));
    let setup = Arc::new(Notify::new());
    let (sink, task) = launch(
        Arc::new(Recognizer {
            first: 42,
            finalize_at_eof: false,
            received: received.clone(),
            setup: Some(setup.clone()),
        }),
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Speaker,
        },
        metrics.clone(),
        origin,
    );
    let (usage, usage_rx) = watch::channel(EndpointUse {
        speaker: true,
        ..Default::default()
    });
    let stop = CancellationToken::new();
    let token = stop.clone();
    let device_open = Arc::new(AtomicBool::new(false));
    let open = device_open.clone();
    let capture = sink.clone();
    let route_metrics = metrics.clone();
    let gate = tokio::spawn(async move {
        activity::while_selected(
            usage_rx,
            TranscriptOrigin::Speaker,
            route_metrics.clone(),
            token,
            |cancel| {
                let capture = capture.clone();
                let open = open.clone();
                let metrics = route_metrics.clone();
                async move {
                    open.store(true, Ordering::SeqCst);
                    capture.submit(frame(origin, 800, 12800, 42), &metrics);
                    cancel.cancelled().await;
                    open.store(false, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !device_open.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    usage.send(EndpointUse::default()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while device_open.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Physical route stops before STT setup finishes");
    assert_eq!(received.load(Ordering::Relaxed), 0);
    setup.notify_one();
    let record = tokio::time::timeout(Duration::from_secs(2), records.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(record, TranscriptRecord::Routed { origin: TranscriptOrigin::Speaker, record } if matches!(&*record, TranscriptRecord::Text { input: true, text, .. } if text == "Original source 42."))
    );
    assert_eq!(received.load(Ordering::Relaxed), 12800);
    assert_eq!(metrics.audio.playback_generation.load(Ordering::Relaxed), 1);
    stop.cancel();
    gate.await.unwrap().unwrap();
    drop(sink);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn merged_file_keeps_both_original_sources_and_eof_finals() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default().transcription;
    config.directory = directory.path().to_str().unwrap().into();
    config.timestamps = true;
    let writer = TranscriptWriter::create_merged(&config, "originals", "fixture", "Originals")
        .await
        .unwrap();
    let (text, records) = mpsc::channel(32);
    let writing = tokio::spawn(writer.run(records));
    let origin = Instant::now();
    let mut tasks = Vec::new();
    for (source, value, end) in [
        (TranscriptOrigin::Microphone, 11, 1000),
        (TranscriptOrigin::Speaker, 22, 5000),
    ] {
        let metrics = Arc::new(RouteMetrics::default());
        let (sink, task) = launch(
            Arc::new(Recognizer {
                first: value,
                finalize_at_eof: true,
                received: Arc::new(AtomicU64::new(0)),
                setup: None,
            }),
            TranscriptSink {
                retained: None,
                sender: text.clone(),
                origin: source,
            },
            metrics.clone(),
            origin,
        );
        sink.submit(frame(origin, end, 16000, value), &metrics);
        drop(sink); // Stop before either provider has acknowledged setup.
        tasks.push(task);
    }
    drop(text);
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    writing.await.unwrap().unwrap();
    let saved = std::fs::read_to_string(directory.path().join("originals.txt")).unwrap();
    assert!(saved.contains("[microphone] [audio +0.000–1.000s] Original source 11."));
    assert!(saved.contains("[received output] [audio +4.000–5.000s] Original source 22."));
    assert!(
        !saved.contains("generated translation") && !saved.contains("reconnection/interruption")
    );
}

#[tokio::test(start_paused = true)]
async fn stop_waits_for_a_full_transcript_queue_without_losing_the_original_final() {
    let (text, mut records) = mpsc::channel(1);
    text.send(TranscriptRecord::Section("Writer backlog".into()))
        .await
        .unwrap();
    let origin = Instant::now();
    let metrics = Arc::new(RouteMetrics::default());
    let (sink, mut task) = launch(
        Arc::new(Recognizer {
            first: 42,
            finalize_at_eof: true,
            received: Arc::new(AtomicU64::new(0)),
            setup: None,
        }),
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        metrics.clone(),
        origin,
    );
    assert!(sink.submit(frame(origin, 100, 1600, 42), &metrics));
    drop(sink);
    assert!(
        tokio::time::timeout(Duration::from_secs(45), &mut task)
            .await
            .is_err()
    );
    assert!(matches!(
        records.recv().await,
        Some(TranscriptRecord::Section(_))
    ));
    assert!(matches!(records.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::Text { ref text, .. }
            if text == "Original source 42.")));
    assert!(matches!(records.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::TurnComplete)));
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(metrics.snapshot().processing_error.is_none());
    assert!(records.recv().await.is_none());
}

#[tokio::test]
async fn stalled_microphone_after_silence_does_not_block_overlapping_output_originals() {
    struct StalledMicrophone {
        stalled: Arc<Notify>,
        resume: Arc<Notify>,
        received: Arc<AtomicU64>,
    }

    #[async_trait]
    impl SpeechProvider for StalledMicrophone {
        fn id(&self) -> &'static str {
            "synthetic-stalled-microphone"
        }

        async fn run(
            &self,
            _config: SessionConfig,
            mut audio: mpsc::Receiver<Vec<i16>>,
            events: mpsc::Sender<ProviderEvent>,
            cancel: CancellationToken,
        ) -> Result<()> {
            events.send(ProviderEvent::Connected).await?;
            ensure!(
                audio.recv().await == Some(vec![0; 1600]),
                "The microphone must retain its original quiet prefix"
            );
            self.stalled.notify_one();
            self.resume.notified().await;
            let mut speech_samples = 0u64;
            while let Some(samples) = audio.recv().await {
                ensure!(
                    samples.iter().all(|sample| *sample == 11),
                    "Microphone STT received speaker or generated audio"
                );
                speech_samples += samples.len() as u64;
                self.received.store(speech_samples, Ordering::Relaxed);
            }
            ensure!(
                !cancel.is_cancelled(),
                "Final microphone text was cancelled"
            );
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Microphone after quiet.".into(),
                    metadata: TranscriptMetadata {
                        start_ms: Some(100),
                        end_ms: Some(100 + speech_samples / 16),
                        ..Default::default()
                    },
                })
                .await?;
            events.send(ProviderEvent::TurnComplete).await?;
            Ok(())
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default().transcription;
    config.directory = directory.path().to_str().unwrap().into();
    config.timestamps = true;
    let writer = TranscriptWriter::create_merged(&config, "overlap", "fixture", "Overlap")
        .await
        .unwrap();
    let (text, records) = mpsc::channel(64);
    let writing = tokio::spawn(writer.run(records));
    let origin = Instant::now();
    let mic_metrics = Arc::new(RouteMetrics::default());
    let output_metrics = Arc::new(RouteMetrics::default());
    let stalled = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let mic_received = Arc::new(AtomicU64::new(0));
    let output_received = Arc::new(AtomicU64::new(0));
    let (mic, mic_task) = launch(
        Arc::new(StalledMicrophone {
            stalled: stalled.clone(),
            resume: resume.clone(),
            received: mic_received.clone(),
        }),
        TranscriptSink {
            retained: None,
            sender: text.clone(),
            origin: TranscriptOrigin::Microphone,
        },
        mic_metrics.clone(),
        origin,
    );
    let (output, output_task) = launch(
        Arc::new(Recognizer {
            first: 22,
            finalize_at_eof: false,
            received: output_received.clone(),
            setup: None,
        }),
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Speaker,
        },
        output_metrics.clone(),
        origin,
    );

    mic.submit(frame(origin, 100, 1600, 0), &mic_metrics);
    tokio::time::timeout(Duration::from_secs(5), stalled.notified())
        .await
        .expect("Microphone recognizer reached its synthetic stall");
    for round in 1..=6 {
        let end_ms = 100 + round * 100;
        mic.submit(frame(origin, end_ms, 1600, 11), &mic_metrics);
        output.submit(frame(origin, end_ms, 1600, 22), &output_metrics);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while output_received.load(Ordering::Relaxed) != 6 * 1600 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Output recognition must progress while microphone recognition is stalled");
    assert_eq!(mic_received.load(Ordering::Relaxed), 0);
    assert!(mic.available() && mic.connected());
    assert_eq!(mic.losses.load(Ordering::Relaxed), 0);
    assert_eq!(output.losses.load(Ordering::Relaxed), 0);

    resume.notify_one();
    mic.submit(frame(origin, 800, 1600, 11), &mic_metrics);
    output.submit(frame(origin, 800, 1600, 22), &output_metrics);
    drop(mic);
    drop(output);
    for task in [mic_task, output_task, writing] {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("Both original recognizers and the merged writer must finalize")
            .unwrap()
            .unwrap();
    }
    assert_eq!(mic_received.load(Ordering::Relaxed), 7 * 1600);
    assert_eq!(output_received.load(Ordering::Relaxed), 7 * 1600);
    let saved = std::fs::read_to_string(directory.path().join("overlap.txt")).unwrap();
    assert!(saved.contains("[microphone] [audio +0.100–0.800s] Microphone after quiet."));
    assert!(saved.contains("[received output] [audio +0.100–0.800s] Original source 22."));
    assert_eq!(saved.matches("Original source 22.").count(), 7);
    assert_eq!(saved.matches("Microphone after quiet.").count(), 1);
    assert!(!saved.contains("generated translation"));
    assert!(!saved.contains("reconnection/interruption"));
}

#[test]
fn timeline_preserves_idle_gaps_and_boundary_sides() {
    let origin = Instant::now();
    let mut timeline = Timeline::default();
    timeline.push(&frame(origin, 2000, 16000, 1), origin);
    timeline.push(&frame(origin, 11000, 16000, 2), origin);
    assert_eq!(timeline.map(0, false), Some(1000));
    assert_eq!(timeline.map(1000, true), Some(2000));
    assert_eq!(timeline.map(1000, false), Some(10000));
    assert_eq!(timeline.map(2000, true), Some(11000));
    assert_eq!(timeline.map(3000, false), None);
}

#[tokio::test]
async fn original_recovery_preserves_pcm_alignment_across_capture_gaps_without_recording_a_gap() {
    struct RecoveringRecognizer {
        resume: Arc<Notify>,
    }

    #[async_trait]
    impl SpeechProvider for RecoveringRecognizer {
        fn id(&self) -> &'static str {
            "synthetic-recovering-stt"
        }

        async fn run(
            &self,
            _config: SessionConfig,
            mut audio: mpsc::Receiver<Vec<i16>>,
            events: mpsc::Sender<ProviderEvent>,
            cancel: CancellationToken,
        ) -> Result<()> {
            events.send(ProviderEvent::Connected).await?;
            ensure!(
                audio.recv().await == Some(vec![11; 1600]),
                "First original changed"
            );
            events
                .send(ProviderEvent::RecoveringOriginal { attempt: 1 })
                .await?;
            self.resume.notified().await;
            events.send(ProviderEvent::Connected).await?;
            // The first final is delayed until recovery, but still describes
            // PCM consumed before recovery rather than a new connection clock.
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Recovered original.".into(),
                    metadata: TranscriptMetadata {
                        alignment_ms: Some(0),
                        ..Default::default()
                    },
                })
                .await?;
            events.send(ProviderEvent::TurnComplete).await?;
            ensure!(
                audio.recv().await == Some(vec![22; 1600]),
                "Queued original changed"
            );
            ensure!(
                audio.recv().await.is_none(),
                "Original audio was duplicated"
            );
            ensure!(
                !cancel.is_cancelled(),
                "Recovery was cancelled before the EOF final"
            );
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Original after capture pause.".into(),
                    metadata: TranscriptMetadata {
                        alignment_ms: Some(100),
                        ..Default::default()
                    },
                })
                .await?;
            events.send(ProviderEvent::TurnComplete).await?;
            Ok(())
        }
    }

    let (text, mut records) = mpsc::channel(8);
    let metrics = Arc::new(RouteMetrics::default());
    let origin = Instant::now();
    let resume = Arc::new(Notify::new());
    let (sink, task) = launch(
        Arc::new(RecoveringRecognizer {
            resume: resume.clone(),
        }),
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        metrics.clone(),
        origin,
    );
    sink.submit(frame(origin, 2100, 1600, 11), &metrics);
    tokio::time::timeout(Duration::from_secs(5), async {
        while metrics.reconnects.load(Ordering::Relaxed) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("The session worker observed the preserved recovery event");
    assert!(!sink.connected());
    assert!(sink.available());
    assert!(
        records.try_recv().is_err(),
        "Recovery alone is not a transcript gap"
    );
    sink.submit(frame(origin, 11100, 1600, 22), &metrics);
    drop(sink);
    resume.notify_one();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for (expected_text, expected_alignment) in [
        ("Recovered original.", 2000),
        ("Original after capture pause.", 11000),
    ] {
        assert!(matches!(records.recv().await,
            Some(TranscriptRecord::Routed { origin: TranscriptOrigin::Microphone, record })
            if matches!(&*record, TranscriptRecord::Text { text, metadata, .. }
                if text == expected_text && metadata.alignment_ms == Some(expected_alignment))));
        assert!(matches!(records.recv().await,
            Some(TranscriptRecord::Routed { record, .. })
            if matches!(*record, TranscriptRecord::TurnComplete)));
    }
    assert!(records.recv().await.is_none());
    assert!(metrics.snapshot().processing_error.is_none());
    assert_eq!(
        metrics
            .audio
            .processing_dropped_frames
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(metrics.audio.playback_generation.load(Ordering::Relaxed), 0);
}

#[tokio::test(start_paused = true)]
async fn stop_drains_every_accepted_original_after_the_previous_finalization_deadlines() {
    struct SlowRecognizer {
        id: &'static str,
        received: Arc<AtomicU64>,
    }

    #[async_trait]
    impl SpeechProvider for SlowRecognizer {
        fn id(&self) -> &'static str {
            self.id
        }

        async fn run(
            &self,
            _config: SessionConfig,
            mut audio: mpsc::Receiver<Vec<i16>>,
            events: mpsc::Sender<ProviderEvent>,
            cancel: CancellationToken,
        ) -> Result<()> {
            events.send(ProviderEvent::Connected).await?;
            while let Some(samples) = audio.recv().await {
                tokio::time::sleep(Duration::from_secs(10)).await;
                ensure!(!cancel.is_cancelled(), "Accepted original was cancelled");
                self.received
                    .fetch_add(samples.len() as u64, Ordering::Relaxed);
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
            ensure!(!cancel.is_cancelled(), "Final recognition was cancelled");
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Every accepted original completed.".into(),
                    metadata: TranscriptMetadata::default(),
                })
                .await?;
            events.send(ProviderEvent::TurnComplete).await?;
            Ok(())
        }
    }

    for id in ["gemini", "synthetic-original-stt"] {
        let (text, mut records) = mpsc::channel(4);
        let metrics = Arc::new(RouteMetrics::default());
        let origin = Instant::now();
        let received = Arc::new(AtomicU64::new(0));
        let (sink, task) = launch(
            Arc::new(SlowRecognizer {
                id,
                received: received.clone(),
            }),
            TranscriptSink {
                retained: None,
                sender: text,
                origin: TranscriptOrigin::Microphone,
            },
            metrics.clone(),
            origin,
        );
        for second in 1..=4 {
            assert!(sink.submit(frame(origin, second * 1000, 16000, 11), &metrics));
        }
        drop(sink);
        let started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("The complete accepted backlog reaches EOF")
            .unwrap()
            .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(50));
        assert_eq!(received.load(Ordering::Relaxed), 64000);
        assert!(matches!(records.recv().await,
            Some(TranscriptRecord::Routed { record, .. })
            if matches!(*record, TranscriptRecord::Text { ref text, .. }
                if text == "Every accepted original completed.")));
        assert!(matches!(records.recv().await,
            Some(TranscriptRecord::Routed { record, .. })
            if matches!(*record, TranscriptRecord::TurnComplete)));
        assert!(records.recv().await.is_none());
    }
}

#[tokio::test]
async fn startup_originals_are_sample_bounded_without_blocking_capture() {
    let config = AppConfig::default();
    let (text, _records) = mpsc::channel(1);
    let metrics = Arc::new(RouteMetrics::default());
    let origin = Instant::now();
    let (sink, pending_worker) = start(
        config,
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Speaker,
        },
        metrics.clone(),
        origin,
    );
    for _ in 0..21 {
        sink.submit(frame(origin, 1000, 16000, 1), &metrics);
    }
    assert_eq!(sink.budget.available_permits(), 0);
    assert_eq!(sink.losses.load(Ordering::Relaxed), 1);
    assert_eq!(
        metrics
            .audio
            .processing_dropped_frames
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(metrics.audio.dropped_frames.load(Ordering::Relaxed), 0);
    drop(pending_worker);
    assert_eq!(sink.budget.available_permits(), QUEUED_SAMPLES);
}

#[tokio::test]
async fn retained_original_backlog_larger_than_the_input_queue_reaches_recognition_after_stop() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let received = Arc::new(AtomicU64::new(0));
    let arrived = entered.clone();
    let proceed = release.clone();
    let counted = received.clone();
    let app = axum::Router::new().route(
        "/inference",
        axum::routing::post(move |body: axum::body::Bytes| {
            let arrived = arrived.clone();
            let proceed = proceed.clone();
            let counted = counted.clone();
            async move {
                let start = body.windows(4).position(|bytes| bytes == b"RIFF").unwrap();
                let mut wav = hound::WavReader::new(std::io::Cursor::new(&body[start..])).unwrap();
                let samples: Vec<_> = wav.samples::<i16>().map(|sample| sample.unwrap()).collect();
                assert!(samples.iter().all(|sample| *sample == 5000));
                if counted.fetch_add(samples.len() as u64, Ordering::Relaxed) == 0 {
                    arrived.notify_one();
                    proceed.notified().await;
                }
                axum::Json(serde_json::json!({"text": "Retained original segment."}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let store = crate::retention::SessionRetention::create_in(directory.path(), "queued-originals")
        .await
        .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=40 {
        originals
            .capture(
                &frame(origin, second * 1000, 16000, 5000),
                TranscriptOrigin::Microphone,
            )
            .unwrap();
    }
    let mut config = AppConfig::default();
    config.transcription.microphone_recognition.provider = "whisper".into();
    config.transcription.providers.whisper.endpoint = endpoint;
    config.transcription.providers.whisper.segment_ms = 5000;
    let metrics = Arc::new(RouteMetrics::default());
    let (text, mut records) = mpsc::channel(32);
    let task = tokio::spawn(start_retained(
        config,
        TranscriptSink {
            retained: Some(originals.clone()),
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        metrics.clone(),
        origin,
        originals.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    originals.close_capture();
    assert!(!task.is_finished());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(received.load(Ordering::Relaxed), 40 * 16000);
    let mut final_segments = 0;
    while let Some(record) = records.recv().await {
        if matches!(record, TranscriptRecord::Routed { record, .. }
            if matches!(*record, TranscriptRecord::Text { .. }))
        {
            final_segments += 1;
        }
    }
    assert!(final_segments > 0);
    assert!(metrics.snapshot().processing_error.is_none());
    assert_eq!(
        metrics
            .audio
            .processing_dropped_frames
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(originals.status().frames, 40);
    server.abort();
}

#[tokio::test]
async fn recognizer_creation_failure_marks_retained_originals_for_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "failed-recognizer")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    originals
        .capture(
            &frame(origin, 100, 1600, 5000),
            TranscriptOrigin::Microphone,
        )
        .unwrap();
    originals.close_capture();
    let mut config = AppConfig::default();
    config.transcription.microphone_recognition.provider = "unsupported-fixture-provider".into();
    let (text, mut records) = mpsc::channel(1);
    let result = start_retained(
        config,
        TranscriptSink {
            retained: Some(originals.clone()),
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        Arc::new(RouteMetrics::default()),
        origin,
        originals.clone(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(originals.status().frames, 1);
    assert!(
        originals
            .status()
            .error
            .unwrap()
            .contains("Original transcription requires recovery")
    );
    assert!(matches!(records.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::Gap)));
}

#[tokio::test(start_paused = true)]
async fn stop_preserves_a_full_startup_queue_until_setup_completes() {
    let origin = Instant::now();
    let metrics = Arc::new(RouteMetrics::default());
    let (text, mut records) = mpsc::channel(4);
    let setup = Arc::new(Notify::new());
    let received = Arc::new(AtomicU64::new(0));
    let (sink, task) = launch(
        Arc::new(Recognizer {
            first: 1,
            finalize_at_eof: true,
            received: received.clone(),
            setup: Some(setup.clone()),
        }),
        TranscriptSink {
            retained: None,
            sender: text,
            origin: TranscriptOrigin::Speaker,
        },
        metrics.clone(),
        origin,
    );
    for _ in 0..QUEUED_FRAMES {
        sink.submit(frame(origin, 1, 1, 1), &metrics);
    }
    while sink.sender.capacity() < QUEUED_FRAMES {
        tokio::task::yield_now().await;
    }
    assert_eq!(sink.losses.load(Ordering::Relaxed), 0);
    drop(sink);
    tokio::time::advance(Duration::from_secs(45)).await;
    assert!(
        !task.is_finished(),
        "Stop must not expire accepted originals during setup"
    );
    setup.notify_one();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("Setup completion drains the full original queue")
        .unwrap()
        .unwrap();
    assert_eq!(received.load(Ordering::Relaxed), QUEUED_FRAMES as u64);
    assert!(matches!(records.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::Text { ref text, .. }
            if text == "Original source 1.")));
    assert!(matches!(records.recv().await,
        Some(TranscriptRecord::Routed { record, .. })
        if matches!(*record, TranscriptRecord::TurnComplete)));
    assert!(records.recv().await.is_none());
}

#[tokio::test]
async fn retained_transcription_retries_only_the_unconfirmed_segment_then_continues() {
    let received = Arc::new(StdMutex::new(Vec::new()));
    let calls = Arc::new(AtomicU64::new(0));
    let recorded = received.clone();
    let counted = calls.clone();
    let app = axum::Router::new().route(
        "/inference",
        axum::routing::post(move |body: axum::body::Bytes| {
            let recorded = recorded.clone();
            let counted = counted.clone();
            async move {
                let start = body.windows(4).position(|bytes| bytes == b"RIFF").unwrap();
                let mut wav = hound::WavReader::new(std::io::Cursor::new(&body[start..])).unwrap();
                let pcm: Vec<i16> = wav.samples().map(|sample| sample.unwrap()).collect();
                let value = pcm[0];
                recorded.lock().unwrap().push(pcm);
                if counted.fetch_add(1, Ordering::Relaxed) == 0 {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(serde_json::json!({"error": "Synthetic interruption"})),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(
                            serde_json::json!({"text": format!("Confirmed source {value}." )}),
                        ),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let directory = tempfile::tempdir().unwrap();
    let store = crate::retention::SessionRetention::create_in(directory.path(), "retry-originals")
        .await
        .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=10 {
        originals
            .capture(
                &frame(
                    origin,
                    second * 1000,
                    16000,
                    if second <= 5 { 5000 } else { 7000 },
                ),
                TranscriptOrigin::Microphone,
            )
            .unwrap();
    }
    originals.close_capture();
    let mut config = AppConfig::default();
    config.transcription.microphone_recognition.provider = "whisper".into();
    config.transcription.providers.whisper.endpoint = endpoint;
    config.transcription.providers.whisper.segment_ms = 5000;
    let metrics = Arc::new(RouteMetrics::default());
    let (text, mut records) = mpsc::channel(32);
    tokio::time::timeout(
        Duration::from_secs(10),
        start_retained(
            config,
            TranscriptSink {
                retained: Some(originals.clone()),
                sender: text,
                origin: TranscriptOrigin::Microphone,
            },
            metrics.clone(),
            origin,
            originals.clone(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        *received.lock().unwrap(),
        vec![vec![5000; 80000], vec![5000; 80000], vec![7000; 80000]]
    );
    let mut saved = Vec::new();
    while let Some(record) = records.recv().await {
        if let TranscriptRecord::Routed { record, .. } = record
            && let TranscriptRecord::Text { text, .. } = *record
        {
            saved.push(text);
        }
    }
    assert_eq!(saved, ["Confirmed source 5000.", "Confirmed source 7000."]);
    assert!(metrics.snapshot().recovering.is_empty());
    assert_eq!(metrics.reconnects.load(Ordering::Relaxed), 1);
    assert!(!originals.capture_requires_stop());
    server.abort();
}

#[derive(Default)]
struct ContextRecognizer {
    inputs: StdMutex<Vec<(String, Vec<i16>)>>,
    confirmed: Notify,
}
#[async_trait]
impl SpeechProvider for ContextRecognizer {
    fn id(&self) -> &'static str {
        "gemini"
    }
    async fn run_history(
        &self,
        config: SessionConfig,
        audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        self.run(config, audio, events, cancel).await
    }
    async fn run(
        &self,
        config: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        _: CancellationToken,
    ) -> Result<()> {
        let mut pcm = Vec::new();
        while let Some(samples) = audio.recv().await {
            pcm.extend(samples);
        }
        self.inputs
            .lock()
            .unwrap()
            .push((config.source_language.clone(), pcm.clone()));
        let Some(_) = pcm.iter().position(|value| value.abs() > 1) else {
            // Exercise the actual typed Gemini timeout, rather than a generic
            // transport failure that must retry the same exact window.
            return Err(crate::provider::missing_transcription_final());
        };
        let mut previous = 0;
        for (position, &value) in pcm.iter().enumerate() {
            if value.abs() > 1 && value != previous {
                events
                    .send(ProviderEvent::Transcript {
                        input: true,
                        text: format!("Confirmed {} source {}.", config.source_language, value),
                        metadata: TranscriptMetadata {
                            alignment_ms: Some(position as u64 * 1000 / u64::from(INPUT_RATE)),
                            ..Default::default()
                        },
                    })
                    .await?;
            }
            previous = value;
        }
        events.send(ProviderEvent::TurnComplete).await?;
        self.confirmed.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn missing_final_extends_only_unconfirmed_originals_and_preserves_both_languages() {
    for (source, language, value) in [
        (TranscriptOrigin::Microphone, "pt-BR", 42),
        (TranscriptOrigin::Speaker, "en-US", -42),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store =
            crate::retention::SessionRetention::create_in(directory.path(), "context-originals")
                .await
                .unwrap();
        let origin = Instant::now();
        let originals =
            retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
        for second in 1..=15 {
            originals
                .capture(
                    &frame(
                        origin,
                        second * 1000,
                        16000,
                        if second <= 5 {
                            1
                        } else if second <= 10 {
                            value
                        } else {
                            90
                        },
                    ),
                    source,
                )
                .unwrap();
        }
        originals.close_capture();
        let config = AppConfig::default();
        let mut settings = provider::stt::session_config(
            &config.transcription.microphone_recognition,
            &config.transcription.providers,
        )
        .unwrap();
        settings.source_language = language.into();
        let processor = Arc::new(ContextRecognizer::default());
        let (text, mut records) = mpsc::channel(32);
        let metrics = Arc::new(RouteMetrics::default());
        run_retained(
            processor.clone(),
            settings,
            TranscriptSink {
                retained: Some(originals.clone()),
                sender: text,
                origin: source,
            },
            metrics.clone(),
            origin,
            originals.clone(),
        )
        .await
        .unwrap();
        {
            let inputs = processor.inputs.lock().unwrap();
            assert_eq!(inputs.len(), 2);
            assert_eq!(inputs[0], (language.into(), vec![1; 80000]));
            assert_eq!(
                inputs[1],
                (
                    language.into(),
                    [vec![1; 80000], vec![value; 80000], vec![90; 80000]].concat()
                )
            );
        }
        let mut saved = Vec::new();
        while let Some(TranscriptRecord::Routed {
            origin: received_source,
            record,
        }) = records.recv().await
        {
            assert_eq!(received_source, source);
            if let TranscriptRecord::Text { text, metadata, .. } = *record {
                saved.push((text, metadata.alignment_ms));
            }
        }
        assert_eq!(
            saved,
            vec![
                (format!("Confirmed {language} source {value}."), Some(5000)),
                (format!("Confirmed {language} source 90."), Some(10000))
            ]
        );
        assert!(metrics.snapshot().processing_error.is_none());
        assert_eq!(originals.status().frames, 15);
        assert!(!originals.status().completed);
    }
}

#[tokio::test]
async fn unconfirmed_context_is_deferred_without_blocking_later_speech_during_capture() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "deferred-originals")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=25 {
        originals
            .capture(
                &frame(
                    origin,
                    second * 1000,
                    16000,
                    if second <= 20 { 1 } else { 42 },
                ),
                TranscriptOrigin::Microphone,
            )
            .unwrap();
    }
    let config = AppConfig::default();
    let mut settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    settings.source_language = "pt-BR".into();
    settings.max_reconnect_attempts = 3;
    let processor = Arc::new(ContextRecognizer::default());
    let (text, mut records) = mpsc::channel(32);
    let metrics = Arc::new(RouteMetrics::default());
    let task = tokio::spawn(run_retained(
        processor.clone(),
        settings,
        TranscriptSink {
            retained: Some(originals.clone()),
            sender: text,
            origin: TranscriptOrigin::Microphone,
        },
        metrics.clone(),
        origin,
        originals.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(6), processor.confirmed.notified())
        .await
        .expect("Later speech must reach recognition while capture is still active");
    assert!(!originals.status().capture_closed);
    originals.close_capture();
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("1 original transcription windows remain unconfirmed")
    );
    assert_eq!(
        processor
            .inputs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, pcm)| pcm.len())
            .collect::<Vec<_>>(),
        [80000, 160000, 240000, 320000, 80000]
    );
    let mut sections = Vec::new();
    let mut saved = Vec::new();
    while let Some(TranscriptRecord::Routed { record, .. }) = records.recv().await {
        match *record {
            TranscriptRecord::Section(text) => sections.push(text),
            TranscriptRecord::Text { text, metadata, .. } => {
                saved.push((text, metadata.alignment_ms))
            }
            _ => {}
        }
    }
    assert_eq!(sections.len(), 1);
    assert!(sections[0].contains("for microphone at +0.000s to +20.000s"));
    assert_eq!(
        saved,
        vec![("Confirmed pt-BR source 42.".into(), Some(20000))]
    );
    assert!(
        metrics
            .snapshot()
            .processing_error
            .unwrap()
            .contains("remain unconfirmed")
    );
    assert!(metrics.snapshot().recovering.is_empty());
    assert_eq!(originals.status().frames, 25);
    assert!(!originals.status().completed);
    assert!(
        originals
            .status()
            .error
            .unwrap()
            .contains("no final acknowledgement")
    );
}

#[tokio::test]
async fn missing_final_context_never_grows_past_thirty_seconds_before_later_speech() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "context-size-limit")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=35 {
        originals
            .capture(
                &frame(
                    origin,
                    second * 1000,
                    16000,
                    if second <= 30 { 1 } else { -42 },
                ),
                TranscriptOrigin::Speaker,
            )
            .unwrap();
    }
    originals.close_capture();
    let config = AppConfig::default();
    let mut settings = provider::stt::session_config(
        &config.transcription.speaker_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    settings.source_language = "en-US".into();
    settings.max_reconnect_attempts = 10;
    let processor = Arc::new(ContextRecognizer::default());
    let (text, mut records) = mpsc::channel(32);
    let metrics = Arc::new(RouteMetrics::default());
    let error = tokio::time::timeout(
        Duration::from_secs(15),
        run_retained(
            processor.clone(),
            settings,
            TranscriptSink {
                retained: Some(originals.clone()),
                sender: text,
                origin: TranscriptOrigin::Speaker,
            },
            metrics,
            origin,
            originals.clone(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("remain unconfirmed"));
    assert_eq!(
        processor
            .inputs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, pcm)| pcm.len())
            .collect::<Vec<_>>(),
        [80000, 480000, 80000]
    );
    let mut found = false;
    while let Some(TranscriptRecord::Routed { record, .. }) = records.recv().await {
        if let TranscriptRecord::Text { text, metadata, .. } = *record {
            assert_eq!(text, "Confirmed en-US source -42.");
            assert_eq!(metadata.alignment_ms, Some(30000));
            found = true;
        }
    }
    assert!(found);
    assert_eq!(originals.status().frames, 35);
    assert!(!originals.status().completed);
}

#[derive(Default)]
struct EndingRecognizer {
    calls: AtomicU64,
    tail_started: Notify,
    release_tail: Notify,
    wait: bool,
}
#[async_trait]
impl SpeechProvider for EndingRecognizer {
    fn id(&self) -> &'static str {
        "gemini"
    }
    async fn run_history(
        &self,
        config: SessionConfig,
        audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        self.run(config, audio, events, cancel).await
    }
    fn is_silent_window(&self, samples: &[i16]) -> bool {
        samples.iter().all(|sample| sample.unsigned_abs() <= 17)
    }
    async fn run(
        &self,
        _: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        _: CancellationToken,
    ) -> Result<()> {
        let mut pcm = Vec::new();
        while let Some(samples) = audio.recv().await {
            pcm.extend(samples);
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        if pcm.iter().all(|value| *value == 42) {
            events
                .send(ProviderEvent::Transcript {
                    input: true,
                    text: "Confirmed speech.".into(),
                    metadata: Default::default(),
                })
                .await?;
            events.send(ProviderEvent::TurnComplete).await?;
            return Ok(());
        }
        self.tail_started.notify_one();
        if self.wait {
            self.release_tail.notified().await;
        }
        Err(crate::provider::missing_transcription_final())
    }
}

#[tokio::test]
async fn confirmed_source_background_tail_finishes_but_unconfirmed_audible_tail_remains_retained() {
    for (tail, expected_calls, succeeds) in [(2, 1, true), (17, 1, true), (1000, 2, false)] {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::retention::SessionRetention::create_in(directory.path(), "closing-tail")
            .await
            .unwrap();
        let origin = Instant::now();
        let originals =
            retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
        for second in 1..=10 {
            originals
                .capture(
                    &frame(
                        origin,
                        second * 1000,
                        16000,
                        if second <= 5 { 42 } else { tail },
                    ),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
        }
        originals.close_capture();
        let config = AppConfig::default();
        let mut settings = provider::stt::session_config(
            &config.transcription.microphone_recognition,
            &config.transcription.providers,
        )
        .unwrap();
        settings.max_reconnect_attempts = 3;
        let processor = Arc::new(EndingRecognizer::default());
        let (sender, mut records) = mpsc::channel(16);
        let metrics = Arc::new(RouteMetrics::default());
        let result = run_retained(
            processor.clone(),
            settings,
            TranscriptSink {
                retained: Some(originals.clone()),
                sender,
                origin: TranscriptOrigin::Microphone,
            },
            metrics.clone(),
            origin,
            originals.clone(),
        )
        .await;
        assert_eq!(result.is_ok(), succeeds);
        assert_eq!(processor.calls.load(Ordering::Relaxed), expected_calls);
        assert_eq!(metrics.snapshot().processing_error.is_none(), succeeds);
        assert_eq!(originals.status().error.is_none(), succeeds);
        assert_eq!(originals.status().frames, 10);
        let mut texts = Vec::new();
        while let Some(TranscriptRecord::Routed { record, .. }) = records.recv().await {
            if let TranscriptRecord::Text { text, .. } = *record {
                texts.push(text);
            }
        }
        assert_eq!(texts, ["Confirmed speech."]);
    }
}

#[tokio::test]
async fn background_after_confirmed_speech_needs_no_remote_request_before_stop() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::retention::SessionRetention::create_in(directory.path(), "closing-inflight-tail")
            .await
            .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=10 {
        originals
            .capture(
                &frame(
                    origin,
                    second * 1000,
                    16000,
                    if second <= 5 { 42 } else { 2 },
                ),
                TranscriptOrigin::Speaker,
            )
            .unwrap();
    }
    let config = AppConfig::default();
    let settings = provider::stt::session_config(
        &config.transcription.speaker_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    let processor = Arc::new(EndingRecognizer {
        wait: true,
        ..Default::default()
    });
    let (sender, mut records) = mpsc::channel(16);
    let metrics = Arc::new(RouteMetrics::default());
    let task = tokio::spawn(run_retained(
        processor.clone(),
        settings,
        TranscriptSink {
            retained: Some(originals.clone()),
            sender,
            origin: TranscriptOrigin::Speaker,
        },
        metrics.clone(),
        origin,
        originals.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(2), records.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!task.is_finished(), "The source remains live until Stop");
    originals.close_capture();
    // Background after recognized speech does not create another remote turn.
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(processor.calls.load(Ordering::Relaxed), 1);
    assert!(metrics.snapshot().processing_error.is_none());
    assert!(metrics.snapshot().recovering.is_empty());
    assert!(originals.status().error.is_none());
    assert_eq!(originals.status().frames, 10);
    while let Some(TranscriptRecord::Routed { record, .. }) = records.recv().await {
        assert!(!matches!(
            *record,
            TranscriptRecord::Gap | TranscriptRecord::Section(_)
        ));
    }
}

#[derive(Default)]
struct CachedRecognizer {
    calls: StdMutex<Vec<i16>>,
    fail_second: AtomicBool,
}
#[async_trait]
impl SpeechProvider for CachedRecognizer {
    fn id(&self) -> &'static str {
        "confirmed-cache-fixture"
    }
    async fn run(
        &self,
        _: SessionConfig,
        _: mpsc::Receiver<Vec<i16>>,
        _: mpsc::Sender<ProviderEvent>,
        _: CancellationToken,
    ) -> Result<()> {
        bail!("Original recognition requires its finite protocol")
    }
    async fn run_history(
        &self,
        _: SessionConfig,
        mut audio: mpsc::Receiver<Vec<i16>>,
        events: mpsc::Sender<ProviderEvent>,
        _: CancellationToken,
    ) -> Result<()> {
        let mut pcm = Vec::new();
        while let Some(samples) = audio.recv().await {
            pcm.extend(samples);
        }
        assert_eq!(pcm.len(), 80000);
        let value = pcm[0];
        assert!(pcm.iter().all(|sample| *sample == value));
        self.calls.lock().unwrap().push(value);
        if value == 2 && self.fail_second.swap(false, Ordering::AcqRel) {
            return Err(provider::permanent_error(anyhow!(
                "Synthetic recognition failure"
            )));
        }
        events
            .send(ProviderEvent::Transcript {
                input: true,
                text: format!("Confirmed original {value}."),
                metadata: TranscriptMetadata {
                    alignment_ms: Some(0),
                    ..Default::default()
                },
            })
            .await?;
        events.send(ProviderEvent::TurnComplete).await?;
        Ok(())
    }
}

#[tokio::test]
async fn recovery_reuses_confirmed_windows_and_transcribes_only_missing_originals() {
    let folder = tempfile::tempdir().unwrap();
    let store = crate::retention::SessionRetention::create_in(folder.path(), "cached-originals")
        .await
        .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 0..15 {
        originals
            .capture(
                &frame(origin, (second + 1) * 1000, 16000, (second / 5 + 1) as i16),
                TranscriptOrigin::Microphone,
            )
            .unwrap();
    }
    originals.close_capture();
    let config = AppConfig::default();
    let settings = provider::stt::session_config(
        &config.transcription.microphone_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    let provider = Arc::new(CachedRecognizer::default());
    provider.fail_second.store(true, Ordering::Release);
    for recovery in [false, true, true] {
        let (text, mut records) = mpsc::channel(32);
        let result = run_retained(
            provider.clone(),
            settings.clone(),
            TranscriptSink {
                sender: text,
                origin: TranscriptOrigin::Microphone,
                retained: Some(originals.clone()),
            },
            Arc::default(),
            origin,
            originals.clone(),
        )
        .await;
        assert_eq!(result.is_ok(), recovery);
        let mut saved = Vec::new();
        while let Some(TranscriptRecord::Routed {
            origin: source,
            record,
        }) = records.recv().await
        {
            assert_eq!(source, TranscriptOrigin::Microphone);
            if let TranscriptRecord::Text { text, metadata, .. } = *record {
                saved.push((text, metadata.alignment_ms));
            }
        }
        let expected = if recovery { 3 } else { 1 };
        assert_eq!(saved.len(), expected);
        for (index, (text, clock)) in saved.iter().enumerate() {
            assert_eq!(text, &format!("Confirmed original {}.", index + 1));
            assert_eq!(*clock, Some(index as u64 * 5000));
        }
    }
    assert_eq!(*provider.calls.lock().unwrap(), vec![1, 2, 2, 3]);
    assert_eq!(originals.status().frames, 15);
    assert!(!originals.status().completed);
}

#[tokio::test]
async fn confirmed_inference_survives_txt_delivery_failure_without_another_model_call() {
    let folder = tempfile::tempdir().unwrap();
    let store = crate::retention::SessionRetention::create_in(folder.path(), "cached-delivery")
        .await
        .unwrap();
    let origin = Instant::now();
    let originals =
        retained::RetainedSession::new(store, origin, &tokio::runtime::Handle::current());
    for second in 1..=5 {
        originals
            .capture(
                &frame(origin, second * 1000, 16000, 1),
                TranscriptOrigin::Speaker,
            )
            .unwrap();
    }
    originals.close_capture();
    let config = AppConfig::default();
    let settings = provider::stt::session_config(
        &config.transcription.speaker_recognition,
        &config.transcription.providers,
    )
    .unwrap();
    let provider = Arc::new(CachedRecognizer::default());
    let (text, records) = mpsc::channel(8);
    drop(records);
    let result = run_retained(
        provider.clone(),
        settings.clone(),
        TranscriptSink {
            sender: text,
            origin: TranscriptOrigin::Speaker,
            retained: Some(originals.clone()),
        },
        Arc::default(),
        origin,
        originals.clone(),
    )
    .await;
    assert!(result.is_err());
    let (text, mut records) = mpsc::channel(8);
    run_retained(
        provider.clone(),
        settings,
        TranscriptSink {
            sender: text,
            origin: TranscriptOrigin::Speaker,
            retained: Some(originals.clone()),
        },
        Arc::default(),
        origin,
        originals,
    )
    .await
    .unwrap();
    assert!(
        matches!(records.recv().await, Some(TranscriptRecord::Routed { record, .. }) if matches!(*record, TranscriptRecord::Text { .. }))
    );
    assert_eq!(*provider.calls.lock().unwrap(), vec![1]);
}
