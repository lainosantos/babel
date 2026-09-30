use super::*;
use crate::{audio::activity::EndpointUse, provider::TranscriptMetadata};
use async_trait::async_trait;

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
async fn startup_originals_are_sample_bounded_without_blocking_capture() {
    let config = AppConfig::default();
    let (text, _records) = mpsc::channel(1);
    let metrics = Arc::new(RouteMetrics::default());
    let origin = Instant::now();
    let (sink, pending_worker) = start(
        config,
        TranscriptSink {
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

#[tokio::test(start_paused = true)]
async fn stop_bounds_unacknowledged_setup_with_a_full_startup_queue() {
    let origin = Instant::now();
    let metrics = Arc::new(RouteMetrics::default());
    let (text, _records) = mpsc::channel(1);
    let (sink, task) = launch(
        Arc::new(Recognizer {
            first: 1,
            finalize_at_eof: true,
            received: Arc::new(AtomicU64::new(0)),
            setup: Some(Arc::new(Notify::new())),
        }),
        TranscriptSink {
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
    let before = tokio::time::Instant::now();
    let error = tokio::time::timeout(FINALIZE_TIMEOUT + Duration::from_secs(1), task)
        .await
        .expect("Closed capture bounds setup even when its queue is full")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("transcript may be incomplete"));
    assert!(before.elapsed() >= FINALIZE_TIMEOUT);
}
