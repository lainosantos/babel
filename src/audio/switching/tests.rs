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
    assert!(error.to_string().contains("dispositivo anterior"));
    assert!(
        received.try_recv().is_err(),
        "a new worker must not start before the old worker exits"
    );
}

// The command service never owns a device. These fixtures exercise the tap at
// the same boundary used by passthrough and translated microphone routes.
struct NoCommandTools;
#[async_trait]
impl crate::commands::CommandTools for NoCommandTools {
    async fn list_tools(&self) -> Result<Vec<crate::commands::CommandTool>> {
        bail!("ordinary fixture speech must not discover tools")
    }
    async fn validate_call(&self, _: &str, _: &serde_json::Value) -> Result<()> {
        bail!("fixture must not validate a tool call")
    }
    async fn call_tool(
        &self,
        _: &str,
        _: serde_json::Value,
        _: &CancellationToken,
    ) -> Result<serde_json::Value> {
        bail!("fixture must not execute a tool")
    }
}

#[tokio::test]
async fn command_tap_backpressure_is_bounded_and_never_mutates_original_pcm() {
    let stats = Arc::new(AudioStats::default());
    let (tx, receiver) = mpsc::channel(8);
    let tap = CommandCaptureTap {
        frames: Some(tx),
        worker: None,
        cleanup: None,
        stats: stats.clone(),
    };
    let frame = OriginalFrame {
        samples: vec![-1.0, -0.1234567, 0.0, 0.5678912, 1.0].into(),
        sample_rate: 48_000,
        channels: 1,
        captured_at: Instant::now(),
    };
    let original = frame.samples.clone();
    for _ in 0..100 {
        tap.frame(frame.clone());
    }
    assert_eq!(receiver.len(), 8);
    assert_eq!(stats.processing_dropped_frames.load(Ordering::Relaxed), 92);
    assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 0);
    assert!(Arc::ptr_eq(&original, &frame.samples));
}

async fn microphone_active(service: &crate::commands::CommandService, active: bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.status().microphone_active != active {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn delayed_command_activation_or_cleanup_cannot_override_a_new_capture() {
    let service = crate::commands::CommandService::new(
        crate::commands::AgentConfig::default(),
        Arc::new(NoCommandTools),
    )
    .unwrap();
    let old = service.begin_capture_scope();
    service.set_microphone_active_scoped(true, old);
    let current = service.begin_capture_scope();
    service.set_microphone_active_scoped(false, current);
    service.set_microphone_active_scoped(true, old);
    assert!(!service.status().microphone_active);
    service.set_microphone_active_scoped(true, current);
    service.set_microphone_active_scoped(false, old);
    assert!(service.status().microphone_active);
    service.set_microphone_active_scoped(false, current);
    assert!(!service.status().microphone_active);
}

#[tokio::test]
async fn microphone_command_tap_deactivates_before_replacement_and_on_cancellation() {
    let service = crate::commands::CommandService::new(
        crate::commands::AgentConfig::default(),
        Arc::new(NoCommandTools),
    )
    .unwrap();
    let (events, mut received) = mpsc::channel(16);
    let backend = MockBackend { events };
    let (devices, selected) = watch::channel("old".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let stats = Arc::new(AudioStats {
        command_tap: Some(Arc::downgrade(&service)),
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "old",
            options(),
            output,
            selected,
            worker_cancel,
            stats,
        )
        .await
    });
    assert_eq!(event(&mut received).await, Event::Started("old".into()));
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[1.0]);
    microphone_active(&service, true).await;
    // A failing replacement emits no frame: inactivity must survive the handoff.
    devices.send_replace("broken".into());
    assert_eq!(event(&mut received).await, Event::Stopped("old".into()));
    microphone_active(&service, false).await;
    assert_eq!(event(&mut received).await, Event::Started("broken".into()));
    microphone_active(&service, false).await;
    devices.send_replace("new".into());
    assert_eq!(event(&mut received).await, Event::Started("new".into()));
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[2.0]);
    microphone_active(&service, true).await;
    cancel.cancel();
    assert_eq!(event(&mut received).await, Event::Stopped("new".into()));
    task.await.unwrap().unwrap();
    microphone_active(&service, false).await;
}

#[tokio::test(start_paused = true)]
async fn speaker_stats_have_no_command_tap_and_do_not_activate_the_agent() {
    let service = crate::commands::CommandService::new(
        crate::commands::AgentConfig::default(),
        Arc::new(NoCommandTools),
    )
    .unwrap();
    let stats = Arc::new(AudioStats::default());
    assert!(stats.command_tap.is_none());
    let (events, mut received) = mpsc::channel(8);
    let backend = MockBackend { events };
    let (_devices, selected) = watch::channel("speaker-monitor".to_owned());
    let (output, mut audio) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "speaker-monitor",
            options(),
            output,
            selected,
            worker_cancel,
            stats,
        )
        .await
    });
    assert_eq!(
        event(&mut received).await,
        Event::Started("speaker-monitor".into())
    );
    assert_eq!(audio.recv().await.unwrap().samples.as_ref(), &[2.0]);
    assert!(!service.status().microphone_active);
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert!(!service.status().microphone_active);
}

struct InjectedCapture {
    input: tokio::sync::Mutex<mpsc::Receiver<OriginalFrame>>,
}
#[async_trait]
impl Backend for InjectedCapture {
    async fn capture(
        &self,
        _: &str,
        _: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        let mut input = self.input.lock().await;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                frame = input.recv() => {
                    let Some(frame) = frame else { return Ok(()); };
                    output.send(frame).await?;
                }
            }
        }
    }
    async fn playback(
        &self,
        _: &str,
        _: AudioOptions,
        _: mpsc::Receiver<PlaybackCommand>,
        _: CancellationToken,
        _: Arc<AudioStats>,
    ) -> Result<()> {
        bail!("capture fixture does not play audio")
    }
}

#[tokio::test]
async fn original_capture_reaches_local_asr_without_waiting_for_its_response() {
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    type PcmReply = Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Vec<i16>>>>>;
    async fn whisper(
        State((reply, response_gate)): State<(PcmReply, Arc<tokio::sync::Notify>)>,
        body: Bytes,
    ) -> Response {
        let Some(start) = body.windows(4).position(|window| window == b"RIFF") else {
            return (
                StatusCode::BAD_REQUEST,
                [(header::SERVER, "whisper.cpp")],
                "Invalid request",
            )
                .into_response();
        };
        let size = u32::from_le_bytes(body[start + 4..start + 8].try_into().unwrap()) as usize + 8;
        let reader =
            hound::WavReader::new(std::io::Cursor::new(&body[start..start + size])).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        let samples = reader
            .into_samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        if let Some(reply) = reply.lock().unwrap().take() {
            let _ = reply.send(samples);
        }
        // A slow ASR must never hold original audio back from its normal route.
        response_gate.notified().await;
        (
            [(header::SERVER, "whisper.cpp")],
            Json(serde_json::json!({"text":"ordinary conversation without the wake name"})),
        )
            .into_response()
    }
    let (reply, original) = tokio::sync::oneshot::channel();
    let response_gate = Arc::new(tokio::sync::Notify::new());
    let router = Router::new()
        .route(
            "/health",
            get(|| async {
                (
                    [(header::SERVER, "whisper.cpp")],
                    Json(serde_json::json!({"status":"ok"})),
                )
            }),
        )
        .route("/inference", post(whisper))
        .with_state((
            Arc::new(std::sync::Mutex::new(Some(reply))),
            response_gate.clone(),
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/inference", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let _server_guard = AbortServerOnDrop(server.abort_handle());
    let config = crate::commands::AgentConfig {
        whisper_endpoint: endpoint,
        needle_endpoint: "http://127.0.0.1:1/complete".into(),
        silence_ms: 200,
        ..Default::default()
    };
    let service = crate::commands::CommandService::new(config, Arc::new(NoCommandTools)).unwrap();
    service.start().unwrap();
    let (input, receiver) = mpsc::channel(4);
    let backend = InjectedCapture {
        input: tokio::sync::Mutex::new(receiver),
    };
    let (_devices, selected) = watch::channel("original-mic".to_owned());
    let (output, mut audio) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let stats = Arc::new(AudioStats {
        command_tap: Some(Arc::downgrade(&service)),
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        capture_with(
            &backend,
            "original-mic",
            AudioOptions {
                sample_rate: 16_000,
                ..options()
            },
            output,
            selected,
            worker_cancel,
            stats,
        )
        .await
    });
    input
        .send(OriginalFrame {
            samples: vec![0.0; 320].into(),
            sample_rate: 16_000,
            channels: 1,
            captured_at: Instant::now(),
        })
        .await
        .unwrap();
    audio.recv().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    for samples in [vec![0.125; 1600], vec![0.0; 3200]] {
        input
            .send(OriginalFrame {
                samples: samples.clone().into(),
                sample_rate: 16_000,
                channels: 1,
                captured_at: Instant::now(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), audio.recv())
                .await
                .unwrap()
                .unwrap()
                .samples
                .as_ref(),
            samples.as_slice()
        );
    }
    let samples = tokio::time::timeout(Duration::from_secs(2), original)
        .await
        .unwrap()
        .unwrap();
    assert!(
        samples
            .iter()
            .filter(|&&sample| (4095..=4096).contains(&sample))
            .count()
            >= 1500
    );
    assert!(
        samples
            .iter()
            .all(|&sample| sample == 0 || (4095..=4096).contains(&sample))
    );
    response_gate.notify_one();
    cancel.cancel();
    task.await.unwrap().unwrap();
    service.shutdown().await;
    assert!(!service.status().microphone_active);
}

struct AbortServerOnDrop(tokio::task::AbortHandle);
impl Drop for AbortServerOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
