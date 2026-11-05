use super::*;
use std::time::Instant;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Started(String),
    Stopped(String),
    Played(String, i16, u64),
}
struct MockBackend {
    events: mpsc::Sender<Event>,
}
#[async_trait]
impl Backend for MockBackend {
    async fn capture(
        &self,
        device: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        self.events.send(Event::Started(device.into())).await?;
        if device == "broken" {
            bail!("mock capture failure");
        }
        output
            .send(OriginalFrame {
                samples: vec![if device == "old" { 1.0 } else { 2.0 }].into(),
                sample_rate: options.sample_rate,
                channels: options.channels,
                captured_at: Instant::now(),
            })
            .await?;
        if device == "stuck" {
            std::future::pending::<()>().await;
        }
        cancel.cancelled().await;
        // Completion after cancellation must be awaited before the replacement.
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.events.send(Event::Stopped(device.into())).await?;
        Ok(())
    }
    async fn playback(
        &self,
        device: &str,
        _: AudioOptions,
        mut input: mpsc::Receiver<PlaybackCommand>,
        cancel: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        self.events.send(Event::Started(device.into())).await?;
        if device == "broken" {
            bail!("mock playback failure");
        }
        if device == "blocked" {
            cancel.cancelled().await;
        } else {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    command = input.recv() => {
                        let Some(command) = command else { bail!("unexpected inner EOF"); };
                        if let PlaybackCommand::Audio { samples, generation } = command {
                            self.events.send(Event::Played(device.into(), samples[0], generation)).await?;
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.events.send(Event::Stopped(device.into())).await?;
        Ok(())
    }
}
fn options() -> AudioOptions {
    AudioOptions {
        sample_rate: 24000,
        channels: 1,
        frame_ms: 20,
        queue_ms: 200,
        latency_ms: 20,
    }
}
async fn event(receiver: &mut mpsc::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
fn command(sample: i16, generation: u64) -> PlaybackCommand {
    PlaybackCommand::Audio {
        samples: vec![sample; 480],
        generation,
    }
}

struct DrainingBackend {
    events: mpsc::Sender<Event>,
    fail: bool,
    draining: Option<Arc<tokio::sync::Notify>>,
}

#[async_trait]
impl Backend for DrainingBackend {
    async fn capture(
        &self,
        _: &str,
        _: AudioOptions,
        _: mpsc::Sender<OriginalFrame>,
        _: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        unreachable!("Playback-only fixture")
    }

    async fn playback(
        &self,
        device: &str,
        _: AudioOptions,
        mut input: mpsc::Receiver<PlaybackCommand>,
        cancel: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        while let Some(command) = input.recv().await {
            tokio::time::sleep(Duration::from_secs(1)).await;
            anyhow::ensure!(
                !cancel.is_cancelled(),
                "Normal EOF cancelled accepted playback"
            );
            if let PlaybackCommand::Audio {
                samples,
                generation,
            } = command
            {
                self.events
                    .send(Event::Played(device.into(), samples[0], generation))
                    .await?;
            }
        }
        // Device buffering can outlive the last accepted input command.
        if let Some(draining) = &self.draining {
            draining.notify_one();
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(5)) => {},
        }
        anyhow::ensure!(
            !cancel.is_cancelled(),
            "Normal EOF interrupted device draining"
        );
        self.events.send(Event::Stopped(device.into())).await?;
        anyhow::ensure!(!self.fail, "Fixture device drain failed");
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn playback_eof_waits_for_the_complete_backend_tail_and_surfaces_drain_failure() {
    for fail in [false, true] {
        let (events, mut received) = mpsc::channel(8);
        let backend = DrainingBackend {
            events,
            fail,
            draining: None,
        };
        let (_device, selected) = watch::channel("output".to_owned());
        let (source, input) = mpsc::channel(8);
        for sample in 1..=4 {
            source.send(command(sample, 0)).await.unwrap();
        }
        drop(source);
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            playback_with(
                &backend,
                "output",
                options(),
                input,
                selected,
                CancellationToken::new(),
                Arc::new(AudioStats::default()),
            ),
        )
        .await
        .unwrap();
        for sample in 1..=4 {
            assert_eq!(
                received.recv().await,
                Some(Event::Played("output".into(), sample, 0))
            );
        }
        assert_eq!(received.recv().await, Some(Event::Stopped("output".into())));
        if fail {
            assert!(format!("{:#}", result.unwrap_err()).contains("Fixture device drain failed"));
        } else {
            result.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn changing_output_during_eof_drain_reports_unfinished_playback() {
    let (events, _received) = mpsc::channel(8);
    let draining = Arc::new(tokio::sync::Notify::new());
    let backend = DrainingBackend {
        events,
        fail: false,
        draining: Some(draining.clone()),
    };
    let (device, selected) = watch::channel("old".to_owned());
    let (source, input) = mpsc::channel(1);
    source.send(command(42, 0)).await.unwrap();
    drop(source);
    let stats = Arc::new(AudioStats::default());
    let observed = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "old",
            options(),
            input,
            selected,
            CancellationToken::new(),
            stats,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), draining.notified())
        .await
        .unwrap();
    device.send("new".into()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Playback device changed before queued audio finished")
    );
    assert_eq!(observed.dropped_frames.load(Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn translated_device_switch_reports_accepted_backend_audio_with_empty_outer_queues() {
    let (events, mut observed) = mpsc::channel(4);
    let backend = MockBackend { events };
    let (device, selected) = watch::channel("old".to_owned());
    let (source, input) = mpsc::channel(1);
    let stats = Arc::new(AudioStats {
        translated_playback: true,
        ..Default::default()
    });
    let checked = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "old",
            options(),
            input,
            selected,
            CancellationToken::new(),
            stats,
        )
        .await
    });
    assert_eq!(event(&mut observed).await, Event::Started("old".into()));
    source.send(command(42, 0)).await.unwrap();
    assert_eq!(
        event(&mut observed).await,
        Event::Played("old".into(), 42, 0)
    );
    assert_eq!(
        source.capacity(),
        1,
        "The outer queue is empty, but the backend accepted audio"
    );
    device.send("new".into()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Translated playback device changed after accepting audio")
    );
    assert_eq!(checked.dropped_frames.load(Ordering::Relaxed), 1);
    assert_eq!(event(&mut observed).await, Event::Stopped("old".into()));
    assert!(
        observed.recv().await.is_none(),
        "Failed translation must not silently restart output"
    );
}

#[tokio::test(start_paused = true)]
async fn capture_congestion_is_distinct_from_playback_discard() {
    let (events, mut received) = mpsc::channel(8);
    let backend = MockBackend { events };
    let (_device, selected) = watch::channel("old".to_owned());
    let (output, _originals) = mpsc::channel(1);
    output
        .send(OriginalFrame {
            samples: vec![0.5].into(),
            sample_rate: options().sample_rate,
            channels: 1,
            captured_at: Instant::now(),
        })
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let stats = Arc::new(AudioStats::default());
    let worker_stats = stats.clone();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move {
        capture_with(
            &backend,
            "old",
            options(),
            output,
            selected,
            worker_cancel,
            worker_stats,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    tokio::time::timeout(Duration::from_secs(1), async {
        while stats.capture_lost_frames.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(stats.capture_lost_frames.load(Ordering::Relaxed), 1);
    discard_audio(command(42, 0), &stats);
    assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 2);
    assert_eq!(stats.capture_lost_frames.load(Ordering::Relaxed), 1);
}

struct BurstBackend;
#[async_trait]
impl Backend for BurstBackend {
    async fn capture(
        &self,
        _: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()> {
        // Deliver a 200 ms device batch in one poll, without waiting for readers.
        for index in 0..20 {
            if output
                .try_send(OriginalFrame {
                    samples: vec![index as f32].into(),
                    sample_rate: options.sample_rate,
                    channels: options.channels,
                    captured_at: Instant::now(),
                })
                .is_err()
            {
                stats.record_capture_loss();
            }
        }
        cancel.cancelled().await;
        Ok(())
    }
    async fn playback(
        &self,
        _: &str,
        _: AudioOptions,
        _: mpsc::Receiver<PlaybackCommand>,
        _: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        unreachable!("Capture-only fixture")
    }
}

#[tokio::test]
async fn configured_capture_window_preserves_batched_device_frames() {
    let (_device, selected) = watch::channel("fixture".to_owned());
    let (output, mut originals) = mpsc::channel(20);
    let cancel = CancellationToken::new();
    let stats = Arc::new(AudioStats::default());
    let worker_cancel = cancel.clone();
    let worker_stats = stats.clone();
    let worker = tokio::spawn(async move {
        capture_with(
            &BurstBackend,
            "fixture",
            AudioOptions {
                frame_ms: 10,
                ..options()
            },
            output,
            selected,
            worker_cancel,
            worker_stats,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        for index in 0..20 {
            assert_eq!(originals.recv().await.unwrap().samples[0], index as f32);
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(stats.capture_lost_frames.load(Ordering::Relaxed), 0);
}

struct ShutdownTailBackend;
#[async_trait]
impl Backend for ShutdownTailBackend {
    async fn capture(
        &self,
        _: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        output
            .send(OriginalFrame {
                samples: vec![1.0].into(),
                sample_rate: options.sample_rate,
                channels: options.channels,
                captured_at: Instant::now(),
            })
            .await?;
        cancel.cancelled().await;
        for sample in [2.0, 3.0] {
            output
                .send(OriginalFrame {
                    samples: vec![sample].into(),
                    sample_rate: options.sample_rate,
                    channels: options.channels,
                    captured_at: Instant::now(),
                })
                .await?;
        }
        Ok(())
    }
    async fn playback(
        &self,
        _: &str,
        _: AudioOptions,
        _: mpsc::Receiver<PlaybackCommand>,
        _: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        unreachable!("Capture-only fixture")
    }
}

#[tokio::test]
async fn capture_shutdown_preserves_backend_tail_after_cancellation() {
    let (_device, selected) = watch::channel("old".to_owned());
    let (output, mut originals) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let stats = Arc::new(AudioStats::default());
    let worker_stats = stats.clone();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move {
        capture_with(
            &ShutdownTailBackend,
            "old",
            options(),
            output,
            selected,
            worker_cancel,
            worker_stats,
        )
        .await
    });
    assert_eq!(originals.recv().await.unwrap().samples.as_ref(), &[1.0]);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for expected in [2.0, 3.0] {
        assert_eq!(
            originals.recv().await.unwrap().samples.as_ref(),
            &[expected]
        );
    }
    assert!(originals.recv().await.is_none());
    assert_eq!(stats.capture_lost_frames.load(Ordering::Relaxed), 0);
}

#[test]
fn mirror_acceptance_never_relabels_old_audio_after_a_concurrent_interruption() {
    let stats = AudioStats::default();
    let audio = command(100, 0);
    let accepted_generation = current_generation(&audio, &stats).unwrap();
    // This is the boundary between accepting the command and creating the
    // publisher/frame. The mirror must keep zero, not reload the newer one.
    stats.playback_generation.store(1, Ordering::Release);
    assert_eq!(accepted_generation, 0);
    assert_ne!(
        accepted_generation,
        stats.playback_generation.load(Ordering::Acquire)
    );
    assert_eq!(current_generation(&audio, &stats), None);
    assert_eq!(current_generation(&PlaybackCommand::Flush, &stats), Some(1));
    let original = PlaybackCommand::Original {
        samples: vec![0.125; 480].into(),
        generation: 0,
    };
    assert_eq!(current_generation(&original, &stats), None);
}

// The driver disappears once, then the SAME endpoint identity becomes available.
struct ReconnectingBackend {
    inner: MockBackend,
    attempts: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl Backend for ReconnectingBackend {
    async fn capture(
        &self,
        device: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.inner
                .events
                .send(Event::Started(device.into()))
                .await?;
            bail!("device disconnected");
        }
        self.inner
            .capture(device, options, output, cancel, stats)
            .await
    }
    async fn playback(
        &self,
        device: &str,
        options: AudioOptions,
        input: mpsc::Receiver<PlaybackCommand>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.inner
                .events
                .send(Event::Started(device.into()))
                .await?;
            bail!("device disconnected");
        }
        self.inner
            .playback(device, options, input, cancel, stats)
            .await
    }
}

#[tokio::test(start_paused = true)]
async fn capture_reconnects_same_identity_without_selection_or_channel_replacement() {
    let (events, mut received) = mpsc::channel(16);
    let backend = ReconnectingBackend {
        inner: MockBackend { events },
        attempts: 0.into(),
    };
    let (_devices, selected) = watch::channel("persistent-usb-microphone".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let observed = stats.clone();
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "persistent-usb-microphone",
            options(),
            output,
            selected,
            token,
            stats,
        )
        .await
    });
    assert_eq!(
        event(&mut received).await,
        Event::Started("persistent-usb-microphone".into())
    );
    tokio::task::yield_now().await;
    tokio::time::advance(RECONNECT_DELAY - Duration::from_millis(1)).await;
    assert!(received.try_recv().is_err(), "retry must back off");
    assert!(observed.capture_error.lock().unwrap().is_some());
    assert!(!audio.is_closed());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        event(&mut received).await,
        Event::Started("persistent-usb-microphone".into())
    );
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[2.0]);
    assert!(observed.capture_error.lock().unwrap().is_none());
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn playback_reconnection_discards_outage_audio_and_keeps_same_endpoint() {
    let (events, mut received) = mpsc::channel(16);
    let backend = ReconnectingBackend {
        inner: MockBackend { events },
        attempts: 0.into(),
    };
    let (_devices, selected) = watch::channel("persistent-usb-headset".to_owned());
    let (input, audio) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let observed = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "persistent-usb-headset",
            options(),
            audio,
            selected,
            token,
            stats,
        )
        .await
    });
    assert_eq!(
        event(&mut received).await,
        Event::Started("persistent-usb-headset".into())
    );
    tokio::task::yield_now().await;
    let outage_generation = observed.playback_generation.load(Ordering::Acquire);
    input.send(command(99, outage_generation)).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(RECONNECT_DELAY).await;
    assert_eq!(
        event(&mut received).await,
        Event::Started("persistent-usb-headset".into())
    );
    let current = observed.playback_generation.load(Ordering::Acquire);
    assert!(current > outage_generation);
    input.send(command(77, outage_generation)).await.unwrap();
    input.send(command(42, current)).await.unwrap();
    assert_eq!(
        event(&mut received).await,
        Event::Played("persistent-usb-headset".into(), 42, current)
    );
    assert!(observed.playback_error.lock().unwrap().is_none());
    assert!(!input.is_closed());
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn capture_replaces_worker_in_order_and_preserves_outer_channel() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("old".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let task = tokio::spawn(async move {
        capture_with(&backend, "old", options(), output, selected, token, stats).await
    });
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[1.0]);
    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Stopped("old".into()));
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[2.0]);
    assert!(!audio.is_closed());
    cancel.cancel();
    assert_eq!(event(&mut received).await, Event::Stopped("new".into()));
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn capture_failure_can_recover_without_ending_the_session() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("broken".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let worker_stats = stats.clone();
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "broken",
            options(),
            output,
            selected,
            token,
            worker_stats,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("broken".into()));
    tokio::task::yield_now().await;
    assert!(
        stats
            .capture_error
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("mock capture failure")
    );
    assert!(!audio.is_closed());
    assert!(!task.is_finished());
    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[2.0]);
    assert!(stats.capture_error.lock().unwrap().is_none());
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn same_device_notification_restarts_and_closed_watch_does_not_spin_or_stop() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("old".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "old",
            options(),
            output,
            selected,
            token,
            Arc::new(AudioStats::default()),
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    audio.recv().await.unwrap();
    devices.send_replace("old".into());
    assert_eq!(event(&mut received).await, Event::Stopped("old".into()));
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    audio.recv().await.unwrap();
    drop(devices);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(!task.is_finished());
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn playback_switch_interrupts_full_inner_queue_and_discards_old_generation() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("blocked".to_owned());
    let (input, audio) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let worker_stats = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "blocked",
            options(),
            audio,
            selected,
            token,
            worker_stats,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("blocked".into()));
    for i in 1..=6 {
        input.send(command(i, 0)).await.unwrap();
    }
    tokio::task::yield_now().await;
    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Stopped("blocked".into()));
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    let generation = stats.playback_generation.load(Ordering::Acquire);
    assert_eq!(generation, 1);
    assert!(stats.dropped_frames.load(Ordering::Relaxed) > 0);
    input.send(command(8, 0)).await.unwrap();
    input.send(command(42, generation)).await.unwrap();
    assert_eq!(
        event(&mut received).await,
        Event::Played("new".into(), 42, generation)
    );
    assert!(!input.is_closed());
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn playback_failure_drains_outage_audio_and_recovers_without_recreating_source() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("broken".to_owned());
    let (input, audio) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stats = Arc::new(AudioStats::default());
    let worker_stats = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "broken",
            options(),
            audio,
            selected,
            token,
            worker_stats,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("broken".into()));
    tokio::task::yield_now().await;
    assert!(
        stats
            .playback_error
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("mock playback failure")
    );
    assert!(!input.is_closed());
    for _ in 0..30 {
        input
            .send(command(
                1,
                stats.playback_generation.load(Ordering::Acquire),
            ))
            .await
            .unwrap();
    }
    tokio::task::yield_now().await;
    assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 30);
    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    assert!(stats.playback_error.lock().unwrap().is_none());
    let generation = stats.playback_generation.load(Ordering::Acquire);
    input.send(command(7, generation)).await.unwrap();
    assert_eq!(
        event(&mut received).await,
        Event::Played("new".into(), 7, generation)
    );
    drop(devices);
    input.send(command(8, generation)).await.unwrap();
    assert_eq!(
        event(&mut received).await,
        Event::Played("new".into(), 8, generation)
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn stuck_previous_worker_never_overlaps_with_replacement() {
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("stuck".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "stuck",
            options(),
            output,
            selected,
            CancellationToken::new(),
            Arc::new(AudioStats::default()),
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("stuck".into()));
    audio.recv().await.unwrap();
    devices.send_replace("new".into());
    let error = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("previous device"));
    assert!(
        received.try_recv().is_err(),
        "a new worker must not start before the old worker exits"
    );
}

#[tokio::test(start_paused = true)]
async fn playback_mirror_preserves_translated_pcm_and_fences_flush_interruptions_and_device_changes()
 {
    let source = super::super::mirror::Source::new();
    let mut mirrored = source.frames.subscribe();
    let mut formats = source.format.subscribe();
    let stats = Arc::new(AudioStats {
        playback_mirror: Some(source.clone()),
        ..Default::default()
    });
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("old".to_owned());
    let (input, commands) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let observed = stats.clone();
    let task = tokio::spawn(async move {
        playback_with(
            &backend,
            "old",
            options(),
            commands,
            selected,
            worker_cancel,
            observed,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    let first_epoch = formats.borrow_and_update().unwrap().epoch;
    let samples = vec![i16::MIN, -1, 0, 1, i16::MAX];
    input
        .send(PlaybackCommand::Audio {
            samples: samples.clone(),
            generation: 0,
        })
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(1), mirrored.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.epoch, first_epoch);
    assert_eq!(first.original.sample_rate, 24_000);
    assert_eq!(first.original.channels, 1);
    assert_eq!(
        first.original.samples.as_ref(),
        samples
            .iter()
            .map(|value| f32::from(*value) / 32768.0)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        event(&mut received).await,
        Event::Played("old".into(), i16::MIN, 0)
    );

    // Interruption must fence the microphone even if no replacement audio arrives.
    stats.playback_generation.fetch_add(1, Ordering::AcqRel);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            formats.changed().await.unwrap();
            if formats
                .borrow_and_update()
                .is_some_and(|format| format.epoch != first_epoch)
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let interrupted_epoch = formats.borrow().unwrap().epoch;
    input.send(command(100, 0)).await.unwrap();
    input.send(PlaybackCommand::Flush).await.unwrap();
    input.send(command(200, 1)).await.unwrap();
    let resumed = tokio::time::timeout(Duration::from_secs(1), mirrored.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        resumed.epoch > interrupted_epoch,
        "Flush must reset the mirror generation"
    );
    assert_eq!(resumed.original.samples[0], 200.0 / 32768.0);
    assert_eq!(
        event(&mut received).await,
        Event::Played("old".into(), 200, 1)
    );
    assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 1);

    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Stopped("old".into()));
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    let generation = stats.playback_generation.load(Ordering::Acquire);
    input.send(command(300, generation)).await.unwrap();
    let replaced = tokio::time::timeout(Duration::from_secs(1), mirrored.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(replaced.epoch > resumed.epoch);
    assert_eq!(replaced.original.samples[0], 300.0 / 32768.0);
    assert_eq!(
        event(&mut received).await,
        Event::Played("new".into(), 300, generation)
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert!(source.format.borrow().is_none());
}
