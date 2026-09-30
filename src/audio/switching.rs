//! Replace device workers without replacing the provider or transcript channels.
use super::{AudioOptions, AudioStats, OriginalFrame, PlaybackCommand};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use std::{
    future::Future,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

#[async_trait]
trait Backend: Send + Sync {
    async fn capture(
        &self,
        device: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()>;
    async fn playback(
        &self,
        device: &str,
        options: AudioOptions,
        input: mpsc::Receiver<PlaybackCommand>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()>;
}
struct SystemBackend;
// A disconnected endpoint can reappear with the same persistent identity. Retry
// only that identity; never silently substitute the OS default device.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);
#[async_trait]
impl Backend for SystemBackend {
    async fn capture(
        &self,
        device: &str,
        options: AudioOptions,
        output: mpsc::Sender<OriginalFrame>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()> {
        super::capture(device, options, output, cancel, stats).await
    }
    async fn playback(
        &self,
        device: &str,
        options: AudioOptions,
        input: mpsc::Receiver<PlaybackCommand>,
        cancel: CancellationToken,
        stats: Arc<AudioStats>,
    ) -> Result<()> {
        super::playback(device, options, input, cancel, stats).await
    }
}

pub async fn capture(
    initial: &str,
    options: AudioOptions,
    output: mpsc::Sender<OriginalFrame>,
    devices: watch::Receiver<String>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    capture_with(
        &SystemBackend,
        initial,
        options,
        output,
        devices,
        cancel,
        stats,
    )
    .await
}
pub async fn playback(
    initial: &str,
    options: AudioOptions,
    input: mpsc::Receiver<PlaybackCommand>,
    devices: watch::Receiver<String>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    playback_with(
        &SystemBackend,
        initial,
        options,
        input,
        devices,
        cancel,
        stats,
    )
    .await
}

fn initial_device(initial: &str, devices: &mut watch::Receiver<String>) -> String {
    let current = devices.borrow_and_update();
    if current.is_empty() {
        initial.to_owned()
    } else {
        current.clone()
    }
}
fn capture_error(stats: &AudioStats, error: Option<String>) {
    *stats
        .capture_error
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = error;
}
fn playback_error(stats: &AudioStats, error: Option<String>) {
    *stats
        .playback_error
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = error;
}
fn error_text(error: &anyhow::Error) -> String {
    format!("{error:#}").chars().take(2048).collect()
}
async fn stop_backend<F: Future<Output = Result<()>> + Unpin>(
    request: &mut F,
    cancel: &CancellationToken,
    finished: bool,
) -> Result<()> {
    cancel.cancel();
    if !finished {
        // Never start a replacement while the previous device still owns I/O.
        // A stuck OS driver is an explicit fatal shutdown error, not overlapping workers.
        let _ = tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .context("The previous device did not stop in time for the switch")?;
    }
    Ok(())
}

enum End {
    Cancelled,
    Changed,
    Disconnected,
    Failed(anyhow::Error),
}

struct CommandCaptureTap {
    frames: Option<mpsc::Sender<OriginalFrame>>,
    worker: Option<tokio::task::JoinHandle<()>>,
    stats: Arc<AudioStats>,
    cleanup: Option<(
        Arc<crate::commands::CommandService>,
        u64,
        tokio::runtime::Handle,
    )>,
}
impl CommandCaptureTap {
    fn new(stats: &Arc<AudioStats>) -> Self {
        let service = stats
            .command_tap
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let mut tap = Self {
            frames: None,
            worker: None,
            stats: stats.clone(),
            cleanup: None,
        };
        if let Some(service) = service
            && let Ok(runtime) = crate::execution::processing_handle()
        {
            let (tx, mut rx) = mpsc::channel::<OriginalFrame>(8);
            let generation = service.begin_capture_scope();
            tap.cleanup = Some((service.clone(), generation, runtime.clone()));
            tap.worker = Some(runtime.spawn(async move {
                service.set_microphone_active_scoped(false, generation);
                let mut active = false;
                let mut speech = super::speech::SpeechTap::new();
                while let Some(frame) = rx.recv().await {
                    if !service.capture_scope_current(generation) {
                        break;
                    }
                    if !active {
                        service.set_microphone_active_scoped(true, generation);
                        active = true;
                    }
                    if service.wants_audio() {
                        let converted = speech.convert(&frame);
                        if service.capture_scope_current(generation) {
                            service.try_audio(&converted);
                        }
                    }
                }
                service.set_microphone_active_scoped(false, generation);
            }));
            tap.frames = Some(tx);
        }
        tap
    }
    fn frame(&self, frame: OriginalFrame) {
        if let Some(frames) = &self.frames
            && frames.try_send(frame).is_err()
        {
            self.stats
                .processing_dropped_frames
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl Drop for CommandCaptureTap {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
        if let Some((service, generation, runtime)) = self.cleanup.take() {
            runtime.spawn(async move {
                service.set_microphone_active_scoped(false, generation);
            });
        }
    }
}

async fn capture_with(
    backend: &dyn Backend,
    initial: &str,
    options: AudioOptions,
    output: mpsc::Sender<OriginalFrame>,
    mut devices: watch::Receiver<String>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let options = options.validate()?;
    let mut selected = initial_device(initial, &mut devices);
    let mut watch_open = true;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        capture_error(&stats, None);
        let worker_cancel = cancel.child_token();
        let _guard = worker_cancel.clone().drop_guard();
        // Device callbacks/IPC deliver bursts, not necessarily one frame per
        // scheduling turn. Reserve the configured, bounded capture window so
        // normal 30–40 ms bursts do not drop 10 ms passthrough frames.
        let (frames, mut incoming) =
            mpsc::channel((options.queue_ms / options.frame_ms).max(1) as usize);
        let mut request = backend.capture(
            &selected,
            options,
            frames,
            worker_cancel.clone(),
            stats.clone(),
        );
        let mut finished = false;
        let mut frame_channel_open = true;
        let command_tap = CommandCaptureTap::new(&stats);
        let mirror = stats
            .original_mirror
            .as_ref()
            .map(|source| source.begin(options));
        let end = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break End::Cancelled,
                _ = output.closed() => break End::Disconnected,
                changed = devices.changed(), if watch_open => {
                    if changed.is_ok() { break End::Changed; }
                    watch_open = false;
                }
                result = &mut request => {
                    finished = true;
                    break End::Failed(result.err().unwrap_or_else(|| anyhow!("Device capture ended unexpectedly")));
                }
                frame = incoming.recv(), if frame_channel_open => {
                    let Some(frame) = frame else { frame_channel_open = false; continue; };
                    // Original frames share immutable storage; publishing to routing
                    // happens before any speech/command work on a separate runtime.
                    let command_frame = frame.clone();
                    match output.try_send(frame) {
                        Ok(()) => (),
                        Err(mpsc::error::TrySendError::Full(_)) => { stats.record_capture_loss(); }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            stats.record_capture_loss();
                            break End::Disconnected;
                        }
                    }
                    if let Some(mirror) = &mirror {
                        mirror.publish(&command_frame);
                    }
                    command_tap.frame(command_frame);
                }
            }
        };
        drop(command_tap);
        drop(mirror);
        let stopped = {
            let stopping = stop_backend(&mut request, &worker_cancel, finished);
            tokio::pin!(stopping);
            let mut incoming_open = true;
            loop {
                tokio::select! {
                    result = &mut stopping => break result,
                    frame = incoming.recv(), if incoming_open => match frame {
                        Some(frame) => forward_capture_tail(frame, &output, &stats),
                        None => incoming_open = false,
                    }
                }
            }
        };
        drop(request);
        incoming.close();
        for _ in 0..incoming.len() {
            if let Ok(frame) = incoming.try_recv() {
                forward_capture_tail(frame, &output, &stats);
            }
        }
        stopped?;
        match end {
            End::Cancelled => return Ok(()),
            End::Disconnected if cancel.is_cancelled() => return Ok(()),
            End::Disconnected => bail!("Capture consumer ended unexpectedly"),
            End::Changed => {
                selected = devices.borrow_and_update().clone();
            }
            End::Failed(error) => {
                tracing::warn!(
                    "Capture unavailable, waiting for reconnection or device selection: {error:#}"
                );
                capture_error(&stats, Some(error_text(&error)));
                // Keep `output` owned here. A failed microphone must not close the
                // provider input or the original transcript for this session.
                let reconnect = tokio::time::sleep(RECONNECT_DELAY);
                tokio::pin!(reconnect);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Ok(()),
                        _ = output.closed() => bail!("Capture consumer ended unexpectedly"),
                        changed = devices.changed(), if watch_open => {
                            if changed.is_ok() { selected = devices.borrow_and_update().clone(); break; }
                            watch_open = false;
                        }
                        _ = &mut reconnect => break,
                    }
                }
            }
        }
    }
}

fn forward_capture_tail(
    frame: OriginalFrame,
    output: &mpsc::Sender<OriginalFrame>,
    stats: &AudioStats,
) {
    // Hardware is stopping. Only original archival consumers may receive this
    // tail; command and mirror publishers have already been revoked.
    if output.try_send(frame).is_err() {
        stats.record_capture_loss();
    }
}

fn discard_audio(command: PlaybackCommand, stats: &AudioStats) {
    if matches!(
        command,
        PlaybackCommand::Audio { .. } | PlaybackCommand::Original { .. }
    ) {
        stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }
}
fn invalidate_playback(
    input: &mut mpsc::Receiver<PlaybackCommand>,
    pending: Option<PlaybackCommand>,
    stats: &AudioStats,
) {
    stats.playback_generation.fetch_add(1, Ordering::AcqRel);
    if let Some(command) = pending {
        discard_audio(command, stats);
    }
    // Drain only the bounded queue present at the handover, not an endless producer.
    for _ in 0..input.len() {
        if let Ok(command) = input.try_recv() {
            discard_audio(command, stats);
        }
    }
}
fn is_current(command: &PlaybackCommand, stats: &AudioStats) -> bool {
    current_generation(command, stats).is_some()
}

/// Retain the command's generation from the same acceptance check. Reading a
/// newer stats generation later must never relabel old PCM as a current mirror.
fn current_generation(command: &PlaybackCommand, stats: &AudioStats) -> Option<u64> {
    let current = stats.playback_generation.load(Ordering::Acquire);
    match command {
        PlaybackCommand::Audio { generation, .. }
        | PlaybackCommand::Original { generation, .. } => {
            (*generation == current).then_some(*generation)
        }
        PlaybackCommand::Flush => Some(current),
    }
}

async fn playback_with(
    backend: &dyn Backend,
    initial: &str,
    options: AudioOptions,
    mut input: mpsc::Receiver<PlaybackCommand>,
    mut devices: watch::Receiver<String>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let options = options.validate()?;
    let mut selected = initial_device(initial, &mut devices);
    let mut watch_open = true;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        playback_error(&stats, None);
        let worker_cancel = cancel.child_token();
        let _guard = worker_cancel.clone().drop_guard();
        let (commands, incoming) = mpsc::channel(2);
        let mut request = backend.playback(
            &selected,
            options,
            incoming,
            worker_cancel.clone(),
            stats.clone(),
        );
        let mut finished = false;
        let mut pending = None;
        let mut mirror = stats
            .playback_mirror
            .as_ref()
            .map(|source| source.begin(options));
        let mut mirror_generation = stats.playback_generation.load(Ordering::Acquire);
        let mut mirror_fence = tokio::time::interval(Duration::from_millis(5));
        mirror_fence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let end = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break End::Cancelled,
                changed = devices.changed(), if watch_open => {
                    if changed.is_ok() { break End::Changed; }
                    watch_open = false;
                }
                result = &mut request => {
                    finished = true;
                    break End::Failed(result.err().unwrap_or_else(|| anyhow!("Device playback ended unexpectedly")));
                }
                _ = mirror_fence.tick(), if mirror.as_ref().is_some_and(super::mirror::Publisher::has_subscribers) => {
                    let generation = stats.playback_generation.load(Ordering::Acquire);
                    if generation != mirror_generation {
                        mirror = stats.playback_mirror.as_ref().map(|source| source.begin(options));
                        mirror_generation = generation;
                    }
                }
                permit = commands.reserve(), if pending.is_some() => {
                    let Ok(permit) = permit else { break End::Failed(anyhow!("The device closed the playback queue")); };
                    let command = pending.take().expect("guarded pending command");
                    if let Some(generation) = current_generation(&command, &stats) {
                        if mirror.is_some() && (generation != mirror_generation || matches!(command, PlaybackCommand::Flush)) {
                            mirror = stats.playback_mirror.as_ref().map(|source| source.begin(options));
                            mirror_generation = generation;
                        }
                        let mirrored = mirror.as_ref().and_then(|mirror| mirror.prepare_playback(&command, options));
                        permit.send(command);
                        if let (Some(mirror), Some(frame)) = (&mirror, mirrored) { mirror.publish(&frame); }
                    } else { discard_audio(command, &stats); }
                }
                command = input.recv(), if pending.is_none() => {
                    let Some(command) = command else { break End::Disconnected; };
                    if is_current(&command, &stats) { pending = Some(command); } else { discard_audio(command, &stats); }
                }
            }
        };
        drop(mirror);
        stop_backend(&mut request, &worker_cancel, finished).await?;
        drop(request);
        drop(commands);
        match end {
            End::Cancelled => return Ok(()),
            End::Disconnected if cancel.is_cancelled() => return Ok(()),
            End::Disconnected => bail!("Playback source ended unexpectedly"),
            End::Changed => {
                invalidate_playback(&mut input, pending, &stats);
                selected = devices.borrow_and_update().clone();
            }
            End::Failed(error) => {
                invalidate_playback(&mut input, pending, &stats);
                tracing::warn!(
                    "Output unavailable, waiting for reconnection or device selection: {error:#}"
                );
                playback_error(&stats, Some(error_text(&error)));
                let reconnect = tokio::time::sleep(RECONNECT_DELAY);
                tokio::pin!(reconnect);
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Ok(()),
                        changed = devices.changed(), if watch_open => {
                            if changed.is_ok() {
                                invalidate_playback(&mut input, None, &stats);
                                selected = devices.borrow_and_update().clone();
                                break;
                            }
                            watch_open = false;
                        }
                        _ = &mut reconnect => {
                            invalidate_playback(&mut input, None, &stats);
                            break;
                        }
                        command = input.recv() => {
                            let Some(command) = command else { bail!("Playback source ended unexpectedly"); };
                            discard_audio(command, &stats);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
