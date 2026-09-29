use super::*;

struct Opened {
    device: String,
    events: mpsc::Sender<&'static str>,
}

struct Harness {
    cancel: CancellationToken,
    usage: watch::Sender<EndpointUse>,
    device: watch::Sender<String>,
    opened: mpsc::UnboundedReceiver<Opened>,
    saved: mpsc::UnboundedReceiver<&'static str>,
    stopped: mpsc::UnboundedReceiver<()>,
    task: JoinHandle<Result<()>>,
    metrics: Arc<RouteMetrics>,
}

impl Harness {
    fn new(origin: TranscriptOrigin) -> Self {
        let (usage, rx) = watch::channel(EndpointUse::default());
        let (device, device_rx) = watch::channel(String::from("physical-one"));
        let cancel = CancellationToken::new();
        let metrics = Arc::new(RouteMetrics::default());
        let (opened_tx, opened) = mpsc::unbounded_channel();
        let (saved_tx, saved) = mpsc::unbounded_channel();
        let (stopped_tx, stopped) = mpsc::unbounded_channel();
        let worker_cancel = cancel.clone();
        let worker_metrics = metrics.clone();
        let task = tokio::spawn(async move {
            // This sender represents the session's long-lived transcript/file
            // writer. Each activation has a separate simulated provider queue.
            while_selected(rx, origin, worker_metrics, worker_cancel, |cancel| {
                let (events, mut incoming) = mpsc::channel(2);
                let writer = saved_tx.clone();
                let stopped = stopped_tx.clone();
                opened_tx
                    .send(Opened {
                        device: device_rx.borrow().clone(),
                        events,
                    })
                    .unwrap();
                async move {
                    loop {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            Some(event) = incoming.recv() => { writer.send(event).unwrap(); },
                        }
                    }
                    stopped.send(()).unwrap();
                    Ok(())
                }
            })
            .await
        });
        Self {
            cancel,
            usage,
            device,
            opened,
            saved,
            stopped,
            task,
            metrics,
        }
    }

    async fn open(&mut self) -> Opened {
        tokio::time::timeout(Duration::from_secs(1), self.opened.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn stop_event(&mut self) {
        tokio::time::timeout(Duration::from_secs(1), self.stopped.recv())
            .await
            .unwrap()
            .unwrap();
    }

    async fn finish(self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn no_microphone_capture_until_an_app_selects_it_and_speaker_changes_do_not_restart_it() {
    let mut h = Harness::new(TranscriptOrigin::Microphone);
    tokio::task::yield_now().await;
    assert!(h.opened.try_recv().is_err());
    assert_eq!(h.metrics.snapshot().state, "waiting_for_app");
    h.usage.send_modify(|u| {
        u.speaker = true;
        u.speaker_epoch += 1;
    });
    tokio::task::yield_now().await;
    assert!(
        h.opened.try_recv().is_err(),
        "speaker must never activate the microphone"
    );
    h.usage.send_modify(|u| {
        u.microphone = true;
        u.microphone_epoch += 1;
    });
    let first = h.open().await;
    h.usage.send_modify(|u| {
        u.speaker = false;
        u.speaker_epoch += 1;
    });
    tokio::task::yield_now().await;
    assert!(h.opened.try_recv().is_err());
    assert!(h.stopped.try_recv().is_err());
    assert!(!first.events.is_closed());
    h.finish().await;
}

#[tokio::test]
async fn switching_away_closes_old_provider_queues_but_preserves_session_and_latest_physical_selection()
 {
    let mut h = Harness::new(TranscriptOrigin::Speaker);
    h.usage.send_modify(|u| {
        u.speaker = true;
        u.speaker_epoch += 1;
    });
    let first = h.open().await;
    first
        .events
        .send("first original transcript")
        .await
        .unwrap();
    assert_eq!(h.saved.recv().await, Some("first original transcript"));
    h.metrics
        .input_level
        .store(0.7_f32.to_bits(), Ordering::Relaxed);
    h.metrics
        .audio
        .passthrough_level
        .store(0.7_f32.to_bits(), Ordering::Relaxed);
    h.usage.send_modify(|u| {
        u.speaker = false;
        u.speaker_epoch += 1;
    });
    h.stop_event().await;
    tokio::task::yield_now().await;
    assert_eq!(h.metrics.snapshot().input_level, 0.0);
    assert!(first.events.send("stale translation").await.is_err());
    assert!(
        matches!(h.saved.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "session writer must stay open"
    );
    h.device.send_replace("replacement-speaker".into());
    h.usage.send_modify(|u| {
        u.speaker = true;
        u.speaker_epoch += 1;
    });
    let next = h.open().await;
    assert_eq!(next.device, "replacement-speaker");
    next.events
        .send("second original transcript")
        .await
        .unwrap();
    assert_eq!(h.saved.recv().await, Some("second original transcript"));
    assert!(h.metrics.audio.playback_generation.load(Ordering::Acquire) > 0);
    h.finish().await;
}

#[tokio::test]
async fn coalesced_deselect_reselect_still_replaces_provider_and_monitor_failure_stops_audio() {
    let mut h = Harness::new(TranscriptOrigin::Speaker);
    h.usage.send_modify(|u| {
        u.speaker = true;
        u.speaker_epoch += 1;
    });
    let first = h.open().await;
    // The consumer sees the final true value, but must not retain old queues.
    h.usage.send_modify(|u| {
        u.speaker = false;
        u.speaker_epoch += 1;
    });
    h.usage.send_modify(|u| {
        u.speaker = true;
        u.speaker_epoch += 1;
    });
    h.stop_event().await;
    let second = h.open().await;
    assert!(first.events.send("late old response").await.is_err());
    assert!(!second.events.is_closed());
    h.usage.send_modify(|u| {
        u.error = Some("Audio server unavailable".into());
        u.speaker_epoch += 1;
    });
    h.stop_event().await;
    tokio::task::yield_now().await;
    assert!(second.events.is_closed());
    assert_eq!(h.metrics.snapshot().state, "waiting_for_app");
    assert_eq!(
        h.metrics.snapshot().device_error.as_deref(),
        Some("Audio server unavailable")
    );
    h.finish().await;
}

#[tokio::test]
async fn reactivation_waits_for_previous_devices_to_close() {
    let (usage, rx) = watch::channel(EndpointUse {
        speaker: true,
        ..Default::default()
    });
    let (opened_tx, mut opened) = mpsc::unbounded_channel();
    let (closing_tx, mut closing) = mpsc::unbounded_channel();
    let (released, release_rx) = watch::channel(false);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        while_selected(
            rx,
            TranscriptOrigin::Speaker,
            Arc::default(),
            task_cancel,
            |cancel| {
                let opened = opened_tx.clone();
                let closing = closing_tx.clone();
                let mut release = release_rx.clone();
                async move {
                    opened.send(()).unwrap();
                    cancel.cancelled().await;
                    closing.send(()).unwrap();
                    if !*release.borrow_and_update() {
                        release.changed().await.unwrap();
                    }
                    Ok(())
                }
            },
        )
        .await
    });
    opened.recv().await.unwrap();
    usage.send_modify(|u| u.speaker_epoch += 2);
    closing.recv().await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        opened.try_recv().is_err(),
        "must not overlap old and new device handles"
    );
    released.send_replace(true);
    opened.recv().await.unwrap();
    drop(usage);
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("Monitor"));
    closing.recv().await.unwrap();
}

#[tokio::test]
async fn cancellation_before_first_activation_never_opens_audio() {
    let (_tx, rx) = watch::channel(EndpointUse {
        speaker: true,
        ..Default::default()
    });
    let cancel = CancellationToken::new();
    cancel.cancel();
    while_selected(
        rx,
        TranscriptOrigin::Speaker,
        Arc::default(),
        cancel,
        |_| async {
            panic!("audio must never be opened after shutdown");
            #[allow(unreachable_code)]
            Ok(())
        },
    )
    .await
    .unwrap();
}
