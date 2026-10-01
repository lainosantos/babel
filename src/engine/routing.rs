//! Original-audio routes live independently of processing/file sessions.
use super::*;
use crate::audio::passthrough::{self, RouteDevices};

pub(super) struct Routing {
    cancel: CancellationToken,
    _cancel_on_drop: tokio_util::sync::DropGuard,
    task: JoinHandle<Result<()>>,
    pub microphone: Arc<RouteMetrics>,
    pub speaker: Arc<RouteMetrics>,
    pub input: watch::Sender<String>,
    pub output: watch::Sender<String>,
    _mic_virtual: watch::Sender<String>,
    _speaker_virtual: watch::Sender<String>,
}
impl Routing {
    pub fn snapshot_route(metrics: &RouteMetrics) -> RouteStatus {
        let mut status = metrics.snapshot();
        let level = f32::from_bits(metrics.audio.passthrough_level.load(Ordering::Relaxed));
        let capture_failed = metrics
            .audio
            .capture_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        let playback_failed = metrics
            .audio
            .playback_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        status.input_level = if capture_failed { 0.0 } else { level };
        status.output_level = if capture_failed || playback_failed {
            0.0
        } else {
            level
        };
        status
    }

    pub fn active(&self) -> bool {
        !self.task.is_finished()
            && [&self.microphone, &self.speaker].iter().any(|metrics| {
                let status = metrics.snapshot();
                status.state == "passthrough" && status.device_error.is_none()
            })
    }
    pub fn error(&self) -> Option<String> {
        let messages = [("microphone", &self.microphone), ("speaker", &self.speaker)]
            .into_iter()
            .filter_map(|(name, metrics)| {
                metrics
                    .snapshot()
                    .device_error
                    .map(|e| format!("{name}: {e}"))
            })
            .collect::<Vec<_>>();
        (!messages.is_empty()).then(|| messages.join("; "))
    }
}

pub(super) async fn stop_routing(state: &mut State) -> Result<()> {
    if let Some(mut routing) = state.routing.take() {
        routing.cancel.cancel();
        match tokio::time::timeout(Duration::from_secs(4), &mut routing.task).await {
            Ok(result) => result.context("Original audio routing interrupted")??,
            Err(_) => {
                routing.task.abort();
                let _ = routing.task.await;
                bail!("Timed out while stopping original audio routing");
            }
        }
    }
    Ok(())
}

pub(super) async fn maintain_routing(state: &mut State) {
    // The physical volume control is useful before virtual drivers exist too.
    // Observing selection does not open an audio stream or require a session.
    if state.routing_enabled && !state.config.speaker.playback_device.is_empty() {
        let _ = endpoint_usage(state);
    }
    if !state.routing_enabled
        || state
            .finalizing
            .iter()
            .any(|session| !session.routes_closed.is_closed())
        || state
            .running
            .as_ref()
            .is_some_and(|running| !running.routes_closed.is_closed())
    {
        return;
    }
    if let Some(routing) = state.routing.as_mut() {
        if !routing.task.is_finished() {
            return;
        }
        state.routing_error = stop_routing(state)
            .await
            .err()
            .map(|error| format!("{error:#}"));
        state.routing_retry_at = Instant::now() + Duration::from_secs(3);
    }
    if Instant::now() < state.routing_retry_at {
        return;
    }
    if let Err(error) = state.config.validate_routing() {
        state.routing_error = Some(error.to_string());
        return;
    }
    if !crate::config::route_configured(&state.config.effective_microphone())
        && !crate::config::route_configured(&state.config.speaker)
    {
        state.routing_error = None;
        return;
    }
    let usage = endpoint_usage(state);
    let cfg = &state.config;
    let (input, input_rx) = watch::channel(cfg.microphone.capture_device.clone());
    let (mic_virtual, mic_virtual_rx) = watch::channel(cfg.microphone.playback_device.clone());
    let (speaker_virtual, speaker_virtual_rx) = watch::channel(cfg.speaker.capture_device.clone());
    let (output, output_rx) = watch::channel(cfg.speaker.playback_device.clone());
    let (microphone, speaker) = RouteMetrics::for_configuration(cfg, &state.commands);
    let mirror_source = speaker.mirror_source();
    let cancel = CancellationToken::new();
    let mut jobs = JoinSet::new();
    let options = AudioOptions {
        sample_rate: 48_000,
        channels: 2,
        frame_ms: 10,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: 80,
    };
    for (route, metrics, capture, playback, origin) in [
        (
            &cfg.microphone,
            &microphone,
            input_rx,
            mic_virtual_rx,
            TranscriptOrigin::Microphone,
        ),
        (
            &cfg.speaker,
            &speaker,
            speaker_virtual_rx,
            output_rx,
            TranscriptOrigin::Speaker,
        ),
    ] {
        if origin == TranscriptOrigin::Microphone
            && let Some(source) = &mirror_source
        {
            jobs.spawn(run_microphone_mirror(
                source.clone(),
                playback,
                cfg.audio.device_latency_ms,
                metrics.clone(),
                cancel.child_token(),
                usage.clone(),
            ));
            continue;
        }
        if !crate::config::route_configured(route) {
            metrics.state("unconfigured");
            continue;
        }
        metrics.state("waiting_for_app");
        let metrics = metrics.clone();
        let route_cancel = cancel.child_token();
        let route_usage = usage.clone();
        let history = state.history.clone();
        jobs.spawn(async move {
            activity::while_selected(
                route_usage,
                origin,
                metrics.clone(),
                route_cancel,
                |active_cancel| {
                    let metrics = metrics.clone();
                    let history = history.clone();
                    let devices = RouteDevices {
                        capture: capture.clone(),
                        playback: playback.clone(),
                    };
                    async move {
                        metrics.state("passthrough");
                        passthrough::run_route_with_history(
                            devices,
                            options,
                            active_cancel,
                            metrics.audio.clone(),
                            history,
                            match origin {
                                TranscriptOrigin::Microphone => RecordingLane::Microphone,
                                TranscriptOrigin::Speaker => RecordingLane::Speaker,
                            },
                        )
                        .await
                    }
                },
            )
            .await
        });
    }
    let worker_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let mut result = tokio::select! {
            biased;
            _ = worker_cancel.cancelled() => Ok(()),
            item = jobs.join_next() => match item {
                Some(Ok(Err(error))) => Err(error),
                _ if worker_cancel.is_cancelled() => Ok(()),
                _ => Err(anyhow!("An original audio route ended unexpectedly")),
            },
        };
        worker_cancel.cancel();
        while let Some(item) = jobs.join_next().await {
            let next = item.context("Routing worker interrupted").and_then(|r| r);
            if result.is_ok() && next.is_err() {
                result = next;
            }
        }
        result
    });
    state.routing = Some(Routing {
        _cancel_on_drop: cancel.clone().drop_guard(),
        cancel,
        task,
        microphone,
        speaker,
        input,
        output,
        _mic_virtual: mic_virtual,
        _speaker_virtual: speaker_virtual,
    });
    state.routing_error = None;
}

pub(super) fn start_monitor(state: std::sync::Weak<Mutex<State>>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { biased; _ = cancel.cancelled() => break, _ = tick.tick() => {} }
            let Some(shared) = state.upgrade() else {
                break;
            };
            let mut state = tokio::select! {
                biased; _ = cancel.cancelled() => break, state = shared.lock() => state,
            };
            state.history.prune(Instant::now());
            reap(&mut state).await;
            maintain_routing(&mut state).await;
        }
    });
}
