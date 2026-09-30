use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use serde_json::Value;
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    AgentConfig, CommandErrorScope, CommandFeedback, CommandPhase, CommandStatus, CommandTools,
    feedback::FeedbackPublisher, history::CommandHistory, inference::Inference,
};
use crate::audio::{PcmFrame, resample::Resampler};

const AUDIO_QUEUE: usize = 32;
const AUDIO_MAX_AGE: Duration = Duration::from_millis(500);
const SEGMENT_MAX_AGE: Duration = Duration::from_secs(2);

struct CapturedAudio {
    frame: PcmFrame,
    // Capture the boundary when this frame enters the queue. Later overload
    // must not invalidate an earlier, already complete utterance.
    drop_epoch: u64,
}

struct Shared {
    config: RwLock<AgentConfig>,
    status: Mutex<CommandStatus>,
    history: Mutex<CommandHistory>,
    feedback: FeedbackPublisher,
    tools: Arc<dyn CommandTools>,
    active: AtomicBool,
    capture_scope: AtomicU64,
    enabled: AtomicBool,
    busy: AtomicBool,
    started: AtomicBool,
    dropped: AtomicU64,
    updates: watch::Sender<u64>,
    revision: AtomicU64,
    current_cancel: Mutex<CancellationToken>,
    shutdown: CancellationToken,
    services_root: Option<PathBuf>,
}

/// A bounded original-microphone tap. Construct outside Tokio if needed, then
/// call `start` to use the dedicated processing runtime. No inference runs on
/// audio workers, even if the caller itself is on the audio runtime.
pub struct CommandService {
    shared: Arc<Shared>,
    audio_tx: mpsc::Sender<CapturedAudio>,
    audio_rx: Mutex<Option<mpsc::Receiver<CapturedAudio>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl CommandService {
    pub fn new(config: AgentConfig, tools: Arc<dyn CommandTools>) -> Result<Arc<Self>> {
        Self::with_services_root(config, tools, None)
    }

    pub fn with_services_root(
        config: AgentConfig,
        tools: Arc<dyn CommandTools>,
        services_root: Option<PathBuf>,
    ) -> Result<Arc<Self>> {
        config.validate()?;
        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_QUEUE);
        let (updates, _) = watch::channel(0);
        Ok(Arc::new(Self {
            shared: Arc::new(Shared {
                status: Mutex::new(CommandStatus::initial(&config)),
                history: Mutex::new(CommandHistory::default()),
                feedback: FeedbackPublisher::new(),
                enabled: AtomicBool::new(config.enabled),
                config: RwLock::new(config),
                tools,
                active: AtomicBool::new(false),
                capture_scope: AtomicU64::new(0),
                busy: AtomicBool::new(false),
                started: AtomicBool::new(false),
                dropped: AtomicU64::new(0),
                updates,
                revision: AtomicU64::new(0),
                current_cancel: Mutex::new(CancellationToken::new()),
                shutdown: CancellationToken::new(),
                services_root,
            }),
            audio_tx,
            audio_rx: Mutex::new(Some(audio_rx)),
            task: Mutex::new(None),
        }))
    }

    pub fn start(&self) -> Result<()> {
        let runtime = crate::execution::processing_handle()?;
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        if task.is_some() {
            return Ok(());
        }
        let Some(receiver) = self
            .audio_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return Ok(());
        };
        self.shared.started.store(true, Ordering::Release);
        *task = Some(runtime.spawn(supervise(self.shared.clone(), receiver)));
        Ok(())
    }

    /// Cheap worker-side gate: skip speech DSP while commands cannot use audio.
    pub(crate) fn wants_audio(&self) -> bool {
        self.shared.started.load(Ordering::Acquire)
            && self.shared.enabled.load(Ordering::Relaxed)
            && self.shared.active.load(Ordering::Relaxed)
            && !self.shared.busy.load(Ordering::Relaxed)
    }

    /// Copies only after reserving bounded queue space, never blocks or waits for
    /// ASR/model/MCP. The caller must supply the physical microphone's original PCM.
    pub fn try_audio(&self, frame: &PcmFrame) -> bool {
        if !self.wants_audio() {
            return false;
        }
        if !(8_000..=192_000).contains(&frame.sample_rate)
            || frame.samples.len() > frame.sample_rate as usize / 5
            || frame.samples.is_empty()
            || frame.captured_at.elapsed() > AUDIO_MAX_AGE
        {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        match self.audio_tx.try_reserve() {
            Ok(permit) => {
                permit.send(CapturedAudio {
                    frame: PcmFrame {
                        samples: frame.samples.clone(),
                        sample_rate: frame.sample_rate,
                        captured_at: frame.captured_at,
                    },
                    drop_epoch: self.shared.dropped.load(Ordering::Acquire),
                });
                true
            }
            Err(_) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// False cancels pending work and drops old speech. Toggle false/true when
    /// switching physical sources so no command spans two different microphones.
    pub fn set_microphone_active(&self, active: bool) {
        self.set_microphone_active_inner(active, None);
    }

    pub(crate) fn begin_capture_scope(&self) -> u64 {
        self.shared.capture_scope.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn capture_scope_current(&self, scope: u64) -> bool {
        self.shared.capture_scope.load(Ordering::Acquire) == scope
    }

    pub(crate) fn set_microphone_active_scoped(&self, active: bool, scope: u64) {
        self.set_microphone_active_inner(active, Some(scope));
    }

    fn set_microphone_active_inner(&self, active: bool, scope: Option<u64>) {
        let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        if scope.is_some_and(|scope| !self.capture_scope_current(scope)) {
            return;
        }
        if self.shared.active.swap(active, Ordering::AcqRel) != active {
            self.shared.changed();
            status.microphone_active = active;
            status.phase = if !self.shared.enabled.load(Ordering::Relaxed) {
                CommandPhase::Disabled
            } else if active {
                CommandPhase::Listening
            } else {
                CommandPhase::Inactive
            };
            status.error = None;
            status.error_scope = None;
            status.sequence = status.sequence.wrapping_add(1);
            self.shared.publish_status(&status);
        }
    }

    pub fn update_config(&self, config: AgentConfig) -> Result<()> {
        config.validate()?;
        self.shared.enabled.store(config.enabled, Ordering::Release);
        *self
            .shared
            .config
            .write()
            .unwrap_or_else(|e| e.into_inner()) = config.clone();
        {
            let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
            let old_sequence = status.sequence;
            let activation = status.activation_id;
            *status = CommandStatus::initial(&config);
            status.sequence = old_sequence.wrapping_add(1);
            status.activation_id = activation;
            status.microphone_active = self.shared.active.load(Ordering::Relaxed);
            self.shared.publish_status(&status);
        }
        self.shared.changed();
        Ok(())
    }

    pub fn cancel(&self) {
        let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            status.phase,
            CommandPhase::Activated
                | CommandPhase::Transcribing
                | CommandPhase::Deciding
                | CommandPhase::Executing
        ) {
            return;
        }
        status.phase = CommandPhase::Failed;
        status.error =
            Some("command cancelled; a dispatched tool may already have completed".into());
        status.error_scope = Some(CommandErrorScope::Command);
        status.sequence = status.sequence.wrapping_add(1);
        self.shared.publish_status(&status);
        self.shared.changed();
    }

    pub fn status(&self) -> CommandStatus {
        let current = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        let mut status = current.clone();
        status.dropped_frames = self.shared.dropped.load(Ordering::Relaxed);
        // Preserve a consistent status/feedback pair if a command transitions
        // while the dashboard is reading the snapshot.
        status.feedback = self.feedback_snapshot();
        status.history_revision = self
            .shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision();
        status
    }

    pub fn history(&self) -> super::CommandHistorySnapshot {
        self.shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot()
    }

    pub fn clear_history(&self) -> super::CommandHistorySnapshot {
        self.shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear()
    }

    pub fn subscribe_feedback(&self) -> broadcast::Receiver<CommandFeedback> {
        self.shared.feedback.subscribe()
    }

    pub fn feedback_snapshot(&self) -> Option<CommandFeedback> {
        self.shared.feedback.snapshot()
    }

    pub async fn shutdown(&self) {
        self.shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interrupt("Babel stopped while a command was pending");
        self.shared.shutdown.cancel();
        self.shared.started.store(false, Ordering::Release);
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut task) = task
            && tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        self.shared.active.store(false, Ordering::Release);
        let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.microphone_active = false;
        status.phase = CommandPhase::Inactive;
        status.error = None;
        status.error_scope = None;
        status.whisper_endpoint = None;
        status.needle_endpoint = None;
        status.sequence = status.sequence.wrapping_add(1);
        self.shared.publish_status(&status);
    }
}

impl Drop for CommandService {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
    }
}

impl Shared {
    fn publish_status(&self, status: &CommandStatus) {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .update(status);
        self.feedback.status_changed(status);
    }

    fn recognition(&self, milliseconds: u64) {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recognition(status.activation_id, milliseconds);
    }

    fn changed(&self) {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interrupt("Command cancelled because microphone routing or agent settings changed");
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.current_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
        self.updates.send_modify(|v| *v = v.wrapping_add(1));
    }
    fn phase(&self, phase: CommandPhase, error: Option<String>) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.phase = phase;
        status.error = error;
        status.error_scope = (phase == CommandPhase::Failed).then_some(CommandErrorScope::Command);
        status.sequence = status.sequence.wrapping_add(1);
        self.publish_status(&status);
    }
    fn service_error(&self, error: String) {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interrupt(&error);
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.phase = CommandPhase::Failed;
        status.error = Some(error);
        status.error_scope = Some(CommandErrorScope::Service);
        status.command = None;
        status.tool = None;
        status.result = None;
        status.sequence = status.sequence.wrapping_add(1);
        self.publish_status(&status);
    }
    fn activate(&self, command: Option<String>) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.phase = CommandPhase::Activated;
        status.activation_id = status.activation_id.wrapping_add(1);
        status.command = command;
        status.error = None;
        status.error_scope = None;
        status.result = None;
        status.tool = None;
        status.sequence = status.sequence.wrapping_add(1);
        self.publish_status(&status);
    }
}

async fn supervise(shared: Arc<Shared>, mut audio: mpsc::Receiver<CapturedAudio>) {
    let mut updates = shared.updates.subscribe();
    let mut services = super::local_services::ManagedServices::new(shared.services_root.clone());
    let mut idle = IdleServices::default();
    loop {
        shared.busy.store(false, Ordering::Release);
        while audio.try_recv().is_ok() {}
        let revision = shared.revision.load(Ordering::Acquire);
        let config = shared
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let active = shared.active.load(Ordering::Relaxed);
        let idle_deadline = idle.deadline(config.enabled && active, config.idle_unload_secs);
        if !config.enabled {
            services.stop().await;
            let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
            status.whisper_endpoint = None;
            status.needle_endpoint = None;
        }
        let phase = if !config.enabled {
            CommandPhase::Disabled
        } else if !active {
            CommandPhase::Inactive
        } else {
            CommandPhase::Listening
        };
        shared.phase(phase, None);
        let epoch = shared.shutdown.child_token();
        *shared
            .current_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = epoch.clone();
        if shared.revision.load(Ordering::Acquire) != revision {
            epoch.cancel();
            continue;
        }
        tokio::select! {
            biased;
            _ = shared.shutdown.cancelled() => { epoch.cancel(); break; }
            _ = updates.changed() => { epoch.cancel(); }
            _ = wait_for_idle(idle_deadline), if config.enabled && !active => {
                services.stop().await;
                idle.unloaded = true;
                let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
                status.whisper_endpoint = None;
                status.needle_endpoint = None;
                status.sequence = status.sequence.wrapping_add(1);
            }
            result = run_epoch(&shared, &config, &mut audio, &epoch, revision, &mut services), if config.enabled && active => {
                epoch.cancel();
                if let Err(error) = result {
                    shared.service_error(error.to_string());
                    let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
                    status.whisper_endpoint = None;
                    status.needle_endpoint = None;
                }
                tokio::select! { _ = shared.shutdown.cancelled() => break, _ = updates.changed() => {}, _ = tokio::time::sleep(Duration::from_secs(3)) => {} }
            }
        }
    }
    services.stop().await;
    shared.busy.store(false, Ordering::Release);
}

#[derive(Default)]
struct IdleServices {
    since: Option<tokio::time::Instant>,
    unloaded: bool,
}

impl IdleServices {
    fn deadline(&mut self, listening: bool, seconds: u32) -> Option<tokio::time::Instant> {
        if listening {
            self.since = None;
            self.unloaded = false;
            return None;
        }
        let since = *self.since.get_or_insert_with(tokio::time::Instant::now);
        (!self.unloaded).then_some(since + Duration::from_secs(seconds.into()))
    }
}

async fn wait_for_idle(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn run_epoch(
    shared: &Shared,
    config: &AgentConfig,
    audio: &mut mpsc::Receiver<CapturedAudio>,
    cancel: &CancellationToken,
    revision: u64,
    services: &mut super::local_services::ManagedServices,
) -> Result<()> {
    let config = services.resolve(config).await?;
    {
        let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.whisper_endpoint = Some(config.whisper_endpoint.clone());
        status.needle_endpoint = Some(config.needle_endpoint.clone());
        status.sequence = status.sequence.wrapping_add(1);
    }
    let inference = Inference::new(&config)?;
    let (segments_tx, segments_rx) = mpsc::channel(1);
    tokio::select! {
        result = collect_audio(shared, &config, audio, segments_tx) => result,
        result = process(shared, &config, inference, segments_rx, cancel, revision) => result,
        result = services.wait_for_exit() => result,
    }
}

struct Segment {
    samples: Vec<i16>,
    finished: Instant,
    truncated: bool,
    drop_epoch: u64,
}

async fn collect_audio(
    shared: &Shared,
    config: &AgentConfig,
    audio: &mut mpsc::Receiver<CapturedAudio>,
    segments: mpsc::Sender<Segment>,
) -> Result<()> {
    let mut segmenter = Segmenter::new(config);
    let mut rate = 0;
    let mut resampler = Resampler::new(16_000, 16_000);
    let epoch_start = Instant::now();
    let mut last_frame = None;
    let mut drop_epoch = shared.dropped.load(Ordering::Acquire);
    let mut continuity_epoch: u64 = 0;
    while let Some(captured) = audio.recv().await {
        let frame = captured.frame;
        if frame.captured_at < epoch_start
            || frame.captured_at.elapsed() > AUDIO_MAX_AGE
            || shared.busy.load(Ordering::Relaxed)
        {
            segmenter = Segmenter::new(config);
            resampler = Resampler::new(frame.sample_rate, 16_000);
            continuity_epoch = continuity_epoch.wrapping_add(1);
            shared.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let current_drops = captured.drop_epoch;
        if current_drops != drop_epoch {
            drop_epoch = current_drops;
            segmenter = Segmenter::new(config);
            resampler = Resampler::new(frame.sample_rate, 16_000);
            continuity_epoch = continuity_epoch.wrapping_add(1);
        }
        if rate != frame.sample_rate
            || last_frame.is_some_and(|last: Instant| {
                frame.captured_at.saturating_duration_since(last) > Duration::from_millis(250)
            })
        {
            rate = frame.sample_rate;
            resampler = Resampler::new(rate, 16_000);
            segmenter = Segmenter::new(config);
            continuity_epoch = continuity_epoch.wrapping_add(1);
        }
        last_frame = Some(frame.captured_at);
        let input: Vec<f32> = frame
            .samples
            .iter()
            .map(|v| f32::from(*v) / 32768.0)
            .collect();
        let mut output = Vec::with_capacity(input.len() * 16_000 / rate as usize + 64);
        resampler.process(&input, &mut output);
        let pcm: Vec<i16> = output
            .into_iter()
            .map(|v| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .collect();
        if let Some((samples, truncated)) = segmenter.push(&pcm)
            && segments
                .try_send(Segment {
                    samples,
                    finished: Instant::now(),
                    truncated,
                    drop_epoch: continuity_epoch,
                })
                .is_err()
        {
            // Frames already admitted before this failure still carry the old
            // capture epoch. Mark the lost utterance immediately so an armed
            // wake cannot accept a later segment across that gap.
            continuity_epoch = continuity_epoch.wrapping_add(1);
            shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

async fn process(
    shared: &Shared,
    config: &AgentConfig,
    inference: Inference,
    mut segments: mpsc::Receiver<Segment>,
    cancel: &CancellationToken,
    revision: u64,
) -> Result<()> {
    let mut activated: Option<tokio::time::Instant> = None;
    let mut asr_failed = false;
    let mut activated_drop_epoch = 0;
    loop {
        let segment = if let Some(until) = activated {
            match tokio::time::timeout_at(until, segments.recv()).await {
                Ok(segment) => segment,
                Err(_) => {
                    activated = None;
                    shared.phase(
                        CommandPhase::Failed,
                        Some("no command followed the wake name".into()),
                    );
                    continue;
                }
            }
        } else {
            segments.recv().await
        };
        let Some(segment) = segment else {
            return Ok(());
        };
        if segment.finished.elapsed() > SEGMENT_MAX_AGE {
            shared.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if activated.is_some() && activated_drop_epoch != segment.drop_epoch {
            activated = None;
            shared.phase(
                CommandPhase::Failed,
                Some("microphone audio was interrupted; repeat the wake name and command".into()),
            );
            continue;
        }
        if segment.truncated {
            if activated.take().is_some() {
                shared.phase(
                    CommandPhase::Failed,
                    Some(
                        "speech exceeded the command duration limit; repeat a shorter command"
                            .into(),
                    ),
                );
            } else {
                // Long ordinary speech is not a failed command. No wake name
                // was recognized, so discard this segment without alerting.
                shared.phase(CommandPhase::Listening, None);
            }
            shared.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if activated.is_some() {
            shared.phase(CommandPhase::Transcribing, None);
        }
        let recognition_started = Instant::now();
        let text = match inference.transcribe(&segment.samples).await {
            Ok(text) => text,
            Err(error) => {
                let command_in_progress = activated.take().is_some();
                asr_failed = true;
                if command_in_progress {
                    shared.phase(CommandPhase::Failed, Some(error.to_string()));
                } else {
                    // activation_id is historical: it must never turn a later
                    // background readiness failure into a new command failure.
                    shared.service_error(error.to_string());
                }
                // A new attempt requires a new completed utterance. Sleeping
                // here only ages the bounded queue and loses prompt retries.
                continue;
            }
        };
        let recognition_ms = recognition_started
            .elapsed()
            .as_millis()
            .min(u64::MAX.into()) as u64;
        if asr_failed {
            shared.phase(CommandPhase::Listening, None);
            asr_failed = false;
        }
        if text.is_empty() {
            continue;
        }
        let command = if let Some(command) = addressed_command(&text, &config.wake_name) {
            shared.activate((!command.is_empty()).then(|| command.clone()));
            if command.is_empty() {
                shared.recognition(recognition_ms);
                activated_drop_epoch = segment.drop_epoch;
                activated = Some(
                    tokio::time::Instant::now()
                        + Duration::from_secs(config.command_window_secs.into()),
                );
                continue;
            }
            command
        } else if activated.take().is_some() {
            text
        } else {
            continue;
        };
        activated = None;
        shared.recognition(recognition_ms);
        {
            let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
            status.command = Some(command.clone());
            shared
                .history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .update(&status);
        }
        if is_cancel(&command) {
            shared.phase(CommandPhase::Failed, Some("command cancelled".into()));
            continue;
        }
        shared.busy.store(true, Ordering::Release);
        let outcome = execute(shared, config, &inference, &command, cancel, revision).await;
        // Ignore audio collected while planning/executing, never replay commands.
        while segments.try_recv().is_ok() {}
        shared.busy.store(false, Ordering::Release);
        if let Err(error) = outcome {
            shared.phase(CommandPhase::Failed, Some(error.to_string()));
        }
    }
}

async fn execute(
    shared: &Shared,
    config: &AgentConfig,
    inference: &Inference,
    command: &str,
    cancel: &CancellationToken,
    revision: u64,
) -> Result<()> {
    {
        let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.command = Some(command.into());
    }
    shared.phase(CommandPhase::Deciding, None);
    let budget = Duration::from_secs(config.timeout_secs.into());
    let tools = tokio::time::timeout(budget, shared.tools.list_tools())
        .await
        .map_err(|_| anyhow::anyhow!("MCP tool discovery timed out"))??;
    let activation = shared
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .activation_id;
    let decision_started = Instant::now();
    let decision = inference.plan(command, &tools).await;
    let decision_ms = decision_started.elapsed().as_millis().min(u64::MAX.into()) as u64;
    {
        let mut history = shared.history.lock().unwrap_or_else(|e| e.into_inner());
        match &decision {
            Ok(decision) => history.decision(
                activation,
                decision.confidence,
                &decision.selected_tools,
                decision_ms,
            ),
            Err(_) => history.decision(activation, None, &[], decision_ms),
        }
    }
    let decision = decision?;
    let calls = decision.calls?;
    for call in &calls {
        tokio::time::timeout(
            budget,
            shared.tools.validate_call(&call.id, &call.arguments),
        )
        .await
        .map_err(|_| anyhow::anyhow!("MCP tool validation timed out"))??;
    }
    let mut results = Vec::new();
    for (index, call) in calls.into_iter().enumerate() {
        anyhow::ensure!(
            !cancel.is_cancelled() && shared.revision.load(Ordering::Acquire) == revision,
            "command cancelled before tool dispatch"
        );
        {
            let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
            status.tool = Some(call.name.clone());
        }
        shared.phase(CommandPhase::Executing, None);
        shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_started(activation, index);
        let result = tokio::time::timeout(
            budget,
            shared.tools.call_tool(&call.id, call.arguments, cancel),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MCP tool timed out; it may already have completed and will not be retried"
            )
        })
        .and_then(|result| result);
        match &result {
            Ok(result) => shared
                .history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .tool_finished(activation, index, Ok(&summarize(result))),
            Err(error) => shared
                .history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .tool_finished(activation, index, Err(&error.to_string())),
        }
        results.push(format!("{}: {}", call.name, summarize(&result?)));
        let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.result = Some(results.join("\n").chars().take(4096).collect());
        shared
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .update(&status);
    }
    {
        let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.result = Some(results.join("\n").chars().take(4096).collect());
    }
    shared.phase(CommandPhase::Succeeded, None);
    Ok(())
}

fn summarize(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.chars().take(2048).collect();
    }
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(2048)
        .collect()
}

fn is_cancel(text: &str) -> bool {
    matches!(
        text.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
            .as_str(),
        "cancel" | "cancelar" | "cancela" | "pare" | "parar" | "stop"
    )
}

/// Only an addressed wake name at the beginning triggers a command. A casual
/// mention of Babel later in a sentence cannot execute an integration.
pub(super) fn addressed_command(text: &str, wake_name: &str) -> Option<String> {
    let words = |value: &str| {
        value
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let wake = words(wake_name);
    if wake.is_empty() {
        return None;
    }
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(begin) = start.take() {
            tokens.push((begin, index, text[begin..index].to_lowercase()));
        }
    }
    if let Some(begin) = start {
        tokens.push((begin, text.len(), text[begin..].to_lowercase()));
    }
    let matches_at = |offset: usize| {
        tokens.len() >= offset + wake.len()
            && wake
                .iter()
                .zip(&tokens[offset..])
                .all(|(expected, token)| expected == &token.2)
    };
    let offset = if matches_at(0) {
        0
    } else if tokens.first().is_some_and(|token| {
        matches!(
            token.2.as_str(),
            "hey" | "oi" | "olá" | "ola" | "ei" | "ok" | "okay"
        )
    }) && matches_at(1)
    {
        1
    } else {
        return None;
    };
    let end = tokens[offset + wake.len() - 1].1;
    Some(
        text[end..]
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim()
            .to_owned(),
    )
}

pub(super) struct Segmenter {
    samples: Vec<i16>,
    preroll: VecDeque<i16>,
    silence: usize,
    voiced: usize,
    max: usize,
    silence_limit: usize,
    threshold: f32,
    discard: bool,
}
impl Segmenter {
    pub fn new(config: &AgentConfig) -> Self {
        Self {
            samples: Vec::with_capacity(config.max_utterance_ms as usize * 16),
            preroll: VecDeque::with_capacity(3200),
            silence: 0,
            voiced: 0,
            max: config.max_utterance_ms as usize * 16,
            silence_limit: config.silence_ms as usize * 16,
            threshold: config.vad_threshold,
            discard: false,
        }
    }
    pub fn push(&mut self, pcm: &[i16]) -> Option<(Vec<i16>, bool)> {
        if pcm.is_empty() {
            return None;
        }
        let rms = (pcm
            .iter()
            .map(|v| (f64::from(*v) / 32768.0).powi(2))
            .sum::<f64>()
            / pcm.len() as f64)
            .sqrt() as f32;
        let speech = rms >= self.threshold;
        if self.discard {
            if speech {
                self.silence = 0;
            } else {
                self.silence += pcm.len();
                if self.silence >= self.silence_limit {
                    self.discard = false;
                    self.silence = 0;
                }
            }
            return None;
        }
        if self.samples.is_empty() && !speech {
            for sample in pcm {
                if self.preroll.len() == 3200 {
                    self.preroll.pop_front();
                }
                self.preroll.push_back(*sample);
            }
            return None;
        }
        if self.samples.is_empty() {
            self.samples.extend(self.preroll.drain(..));
        }
        if speech {
            self.voiced += pcm.len();
            self.silence = 0;
        } else {
            self.silence += pcm.len();
        }
        let available = self.max.saturating_sub(self.samples.len());
        self.samples
            .extend_from_slice(&pcm[..pcm.len().min(available)]);
        if self.samples.len() >= self.max {
            self.samples.clear();
            self.voiced = 0;
            self.silence = 0;
            self.discard = true;
            return Some((Vec::new(), true));
        }
        if self.silence >= self.silence_limit {
            let audible = self.voiced >= 1600;
            self.silence = 0;
            self.voiced = 0;
            let samples = std::mem::replace(&mut self.samples, Vec::with_capacity(self.max));
            if audible {
                return Some((samples, false));
            }
        }
        None
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    struct UnusedTools;

    #[async_trait::async_trait]
    impl CommandTools for UnusedTools {
        async fn list_tools(&self) -> Result<Vec<super::super::CommandTool>> {
            unreachable!("collector test does not invoke tools")
        }

        async fn validate_call(&self, _: &str, _: &Value) -> Result<()> {
            unreachable!("collector test does not invoke tools")
        }

        async fn call_tool(&self, _: &str, _: Value, _: &CancellationToken) -> Result<Value> {
            unreachable!("collector test does not invoke tools")
        }
    }

    #[tokio::test]
    async fn full_segment_queue_breaks_continuity_for_already_admitted_audio() {
        let config = AgentConfig {
            silence_ms: 200,
            max_utterance_ms: 1000,
            ..Default::default()
        };
        let service = CommandService::new(config.clone(), Arc::new(UnusedTools)).unwrap();
        let mut audio = service.audio_rx.lock().unwrap().take().unwrap();
        let (segments, mut received) = mpsc::channel(1);
        let collector = collect_audio(&service.shared, &config, &mut audio, segments);
        tokio::pin!(collector);
        let exercise = async {
            // These frames all carry the same admission epoch. The first
            // utterance fills the segment queue; the second cannot be retained.
            for level in [3000, 0, 3000, 0] {
                service
                    .audio_tx
                    .send(CapturedAudio {
                        frame: PcmFrame {
                            samples: vec![level; if level == 0 { 3200 } else { 1600 }],
                            sample_rate: 16000,
                            captured_at: Instant::now(),
                        },
                        drop_epoch: 0,
                    })
                    .await
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while service.shared.dropped.load(Ordering::Acquire) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let before_loss = received.recv().await.unwrap();
            // Simulate frames already admitted before the previous segment was
            // dropped. Freeing the segment queue must not erase that boundary.
            for level in [3000, 0] {
                service
                    .audio_tx
                    .send(CapturedAudio {
                        frame: PcmFrame {
                            samples: vec![level; if level == 0 { 3200 } else { 1600 }],
                            sample_rate: 16000,
                            captured_at: Instant::now(),
                        },
                        drop_epoch: 0,
                    })
                    .await
                    .unwrap();
            }
            let after_loss = tokio::time::timeout(Duration::from_secs(3), received.recv())
                .await
                .unwrap()
                .unwrap();
            assert_ne!(before_loss.drop_epoch, after_loss.drop_epoch);
            assert_eq!(service.shared.dropped.load(Ordering::Acquire), 1);
        };
        // Poll collection first so its epoch starts before synthetic capture.
        tokio::select! {
            biased;
            result = &mut collector => panic!("collector stopped unexpectedly: {result:?}"),
            _ = exercise => {}
        }
    }

    #[tokio::test(start_paused = true)]
    async fn microphone_return_cancels_unload_and_inactive_updates_do_not_extend_the_grace() {
        let mut idle = IdleServices::default();
        assert!(idle.deadline(true, 60).is_none());
        let first = idle.deadline(false, 60).unwrap();
        tokio::time::advance(Duration::from_secs(45)).await;
        assert_eq!(idle.deadline(false, 60), Some(first));
        assert!(idle.deadline(true, 60).is_none());
        let next = idle.deadline(false, 60).unwrap();
        assert!(next > first);
        // Reducing the limit applies to the original idle start, not now.
        assert_eq!(
            idle.deadline(false, 10),
            Some(next - Duration::from_secs(50))
        );
        wait_for_idle(idle.deadline(false, 10)).await;
        assert_eq!(tokio::time::Instant::now(), next - Duration::from_secs(50));
        idle.unloaded = true;
        assert!(idle.deadline(false, 10).is_none());
        assert!(idle.deadline(true, 10).is_none());
        assert!(idle.deadline(false, 10).is_some());
    }
}
