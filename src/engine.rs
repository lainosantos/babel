mod activity;
mod agent;
mod command_tools;
mod history;
mod notifications;
mod routing;
use routing::{Routing, maintain_routing, stop_routing};

use crate::{
    audio::{self, AudioOptions, AudioStats, DeviceDirection, PlaybackCommand},
    config::{AppConfig, RouteConfig},
    provider::{self, ProviderEvent, SessionConfig},
    recording::{AudioRecord, RecordingLane, SessionAudioRecorder},
    transcript::{TranscriptOrigin, TranscriptRecord, TranscriptWriter},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

const INPUT_RATE: u32 = 16_000;
const OUTPUT_RATE: u32 = 24_000;
const OUTPUT_FRAME_MS: u32 = 20;
const OUTPUT_FRAME_SAMPLES: usize = 480;

#[derive(Debug)]
pub struct ConfigurationChanged;
impl std::fmt::Display for ConfigurationChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("A configuração foi alterada fora deste painel. Recarregue os ajustes antes de salvar ou iniciar.")
    }
}
impl std::error::Error for ConfigurationChanged {}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RouteStatus {
    pub state: String,
    pub captured_frames: u64,
    pub dropped_frames: u64,
    pub underruns: u64,
    pub translated_samples: u64,
    pub reconnects: u64,
    pub input_level: f32,
    pub output_level: f32,
    pub last_input_transcript: Option<String>,
    pub last_output_transcript: Option<String>,
    pub device_error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct EngineStatus {
    pub running: bool,
    pub routing_active: bool,
    pub routing_error: Option<String>,
    pub config_revision: u64,
    pub session_name: Option<String>,
    pub session_id: Option<String>,
    pub microphone: RouteStatus,
    pub speaker: RouteStatus,
    pub last_error: Option<String>,
    pub local_runtime: crate::local_runtime::RuntimeStatus,
    pub history: crate::history::HistoryStatus,
    pub history_included_secs: f64,
    pub history_transcription_pending: bool,
}

#[derive(Default)]
struct RouteMetrics {
    audio: Arc<AudioStats>,
    view: StdMutex<RouteStatus>,
    translated_samples: AtomicU64,
    reconnects: AtomicU64,
    input_level: AtomicU32,
    output_level: AtomicU32,
    activity_error: StdMutex<Option<String>>,
}
impl RouteMetrics {
    fn with_commands(commands: &Arc<crate::commands::CommandService>) -> Self {
        Self {
            audio: Arc::new(AudioStats {
                command_tap: Some(Arc::downgrade(commands)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn state(&self, state: &str) {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).state = state.into();
    }
    fn snapshot(&self) -> RouteStatus {
        let mut status = self.view.lock().unwrap_or_else(|e| e.into_inner()).clone();
        status.captured_frames = self.audio.captured_frames.load(Ordering::Relaxed);
        status.dropped_frames = self.audio.dropped_frames.load(Ordering::Relaxed);
        status.underruns = self.audio.underruns.load(Ordering::Relaxed);
        status.translated_samples = self.translated_samples.load(Ordering::Relaxed);
        status.reconnects = self.reconnects.load(Ordering::Relaxed);
        status.input_level = f32::from_bits(self.input_level.load(Ordering::Relaxed));
        status.output_level = f32::from_bits(self.output_level.load(Ordering::Relaxed));
        let capture = self
            .audio
            .capture_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let playback = self
            .audio
            .playback_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let activity = self
            .activity_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let errors = [capture, playback, activity]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        status.device_error = (!errors.is_empty()).then(|| errors.join("; "));
        status
    }
}

struct Running {
    _cancel_on_drop: tokio_util::sync::DropGuard,
    _local_runtime: Option<crate::local_runtime::RuntimeLease>,
    session: crate::session::SessionIdentity,
    cancel: CancellationToken,
    task: JoinHandle<Result<()>>,
    microphone: Arc<RouteMetrics>,
    speaker: Arc<RouteMetrics>,
    physical_input: watch::Sender<String>,
    physical_output: watch::Sender<String>,
    history_included_secs: f64,
    history_transcription_pending: Arc<AtomicBool>,
}
impl Running {
    fn snapshot(&self) -> EngineStatus {
        let microphone = self.microphone.snapshot();
        let speaker = self.speaker.snapshot();
        let errors = [
            ("microfone", &microphone.device_error),
            ("saída", &speaker.device_error),
        ]
        .into_iter()
        .filter_map(|(route, error)| {
            error
                .as_ref()
                .map(|error| format!("Dispositivo de {route}: {error}"))
        })
        .collect::<Vec<_>>();
        EngineStatus {
            running: !self.task.is_finished(),
            routing_active: !self.task.is_finished() && [&microphone, &speaker].iter().any(|route| {
                !matches!(route.state.as_str(), "waiting_for_app" | "unconfigured" | "stopped" | "error")
                    && route.device_error.is_none()
            }),
            routing_error: None,
            config_revision: 0,
            session_name: Some(self.session.name.clone()),
            session_id: Some(self.session.id.clone()),
            microphone,
            speaker,
            last_error: (!errors.is_empty()).then(|| format!("{}. Escolha outro dispositivo físico pela bandeja para continuar na mesma sessão.", errors.join("; "))),
            local_runtime: Default::default(),
            history: Default::default(),
            history_included_secs: self.history_included_secs,
            history_transcription_pending: self.history_transcription_pending.load(Ordering::Acquire),
        }
    }
}

struct State {
    config: AppConfig,
    history: Arc<crate::history::HistoryBuffer>,
    config_revision: u64,
    agent_revision: u64,
    commands: Arc<crate::commands::CommandService>,
    running: Option<Running>,
    last: EngineStatus,
    routing_enabled: bool,
    routing_monitor_started: bool,
    routing: Option<Routing>,
    routing_error: Option<String>,
    routing_retry_at: Instant,
    pending_start: Option<(u64, CancellationToken)>,
    next_start_id: u64,
}

/// Serializes lifecycle transitions; only non-real-time control paths use this lock.
pub struct Controller {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    monitor_cancel: CancellationToken,
    commands: Arc<crate::commands::CommandService>,
    mcp: Arc<crate::mcp_client::McpClient>,
    command_tools: Arc<command_tools::AgentTools>,
    notifications_started: std::sync::atomic::AtomicBool,
    local_runtime: Arc<crate::local_runtime::RuntimeManager>,
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.monitor_cancel.cancel();
    }
}
impl Controller {
    pub fn config_path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn file_paths(
        &self,
        base_path: &str,
        transcription_directory: &str,
        recording_directory: &str,
    ) -> Result<crate::storage::FilePaths> {
        crate::storage::resolve(base_path, transcription_directory, recording_directory)
    }
    pub fn new(config: AppConfig, path: PathBuf) -> Result<Self> {
        let local_runtime = crate::local_runtime::RuntimeManager::new();
        local_runtime.reconcile(&config);
        let mcp = Arc::new(crate::mcp_client::McpClient::new());
        let command_tools = Arc::new(command_tools::AgentTools::new(
            mcp.clone(),
            config.agent.integrations.clone(),
        ));
        let absolute_config = std::path::absolute(&path)?;
        let commands = crate::commands::CommandService::with_services_root(
            config.agent.clone(),
            command_tools.clone(),
            absolute_config.parent().map(ToOwned::to_owned),
        )?;
        Ok(Self {
            path,
            commands: commands.clone(),
            mcp,
            command_tools,
            notifications_started: std::sync::atomic::AtomicBool::new(false),
            local_runtime,
            monitor_cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State {
                history: Arc::new(crate::history::HistoryBuffer::new(&config.history)),
                config,
                config_revision: 0,
                agent_revision: 0,
                commands,
                running: None,
                last: stopped_status(),
                routing_enabled: false,
                routing_monitor_started: false,
                routing: None,
                routing_error: None,
                routing_retry_at: Instant::now(),
                pending_start: None,
                next_start_id: 0,
            })),
        })
    }
    /// Enable original-audio routing for the lifetime of the app, without AI or files.
    pub async fn enable_routing(&self) -> Result<()> {
        self.commands.start()?;
        self.start_command_notifications();
        let mut state = self.state.lock().await;
        state.config.validate_routing()?;
        self.local_runtime.reconcile(&state.config);
        state.routing_enabled = true;
        if !state.routing_monitor_started {
            state.routing_monitor_started = true;
            routing::start_monitor(Arc::downgrade(&self.state), self.monitor_cancel.clone());
        }
        maintain_routing(&mut state).await;
        Ok(())
    }
    /// Exit the app: unlike ending a session, this also closes the audio bridges.
    pub async fn shutdown(&self) -> Result<()> {
        {
            self.state.lock().await.routing_enabled = false;
        }
        self.monitor_cancel.cancel();
        let session = self.stop().await;
        let routing = stop_routing(&mut *self.state.lock().await).await;
        self.commands.shutdown().await;
        self.local_runtime.shutdown().await;
        session.and(routing)
    }
    pub async fn install_virtual_devices(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        ensure!(
            state.running.is_none(),
            "Encerre a sessão antes de criar dispositivos virtuais"
        );
        stop_routing(&mut state).await?;
        let result = audio::install_virtual_devices().await;
        state.routing_enabled = state.routing_monitor_started;
        state.routing_retry_at = Instant::now();
        maintain_routing(&mut state).await;
        result
    }
    pub async fn uninstall_virtual_devices(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        ensure!(
            state.running.is_none(),
            "Encerre a sessão antes de remover dispositivos virtuais"
        );
        stop_routing(&mut state).await?;
        state.routing_enabled = false;
        audio::uninstall_virtual_devices().await
    }
    pub async fn report_error(&self, error: String) {
        self.state.lock().await.last.last_error = Some(error);
    }
    pub async fn config(&self) -> AppConfig {
        self.state.lock().await.config.clone()
    }
    pub async fn config_snapshot(&self) -> (AppConfig, u64) {
        let state = self.state.lock().await;
        (state.config.clone(), state.config_revision)
    }
    pub async fn interface_snapshot(&self) -> (crate::config::InterfaceConfig, u64) {
        let state = self.state.lock().await;
        (state.config.interface.clone(), state.config_revision)
    }
    pub async fn status(&self) -> EngineStatus {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        let mut status = state
            .running
            .as_ref()
            .map_or_else(|| state.last.clone(), Running::snapshot);
        if state.running.is_none() {
            if let Some(routing) = &state.routing {
                status.routing_active = routing.active();
                status.routing_error = routing.error();
                status.microphone = Routing::snapshot_route(&routing.microphone);
                status.speaker = Routing::snapshot_route(&routing.speaker);
            } else {
                status.routing_active = false;
            }
        }
        if status.routing_error.is_none() {
            status.routing_error = state.routing_error.clone();
        }
        status.config_revision = state.config_revision;
        status.local_runtime = self.local_runtime.status();
        status.history = state.history.status(Instant::now());
        if status.running && status.last_error.is_none() {
            status.last_error = state.last.last_error.clone();
        }
        status
    }
    pub async fn set_config(&self, config: AppConfig) -> Result<()> {
        self.set_config_if_revision(config, None).await
    }
    /// Persist a presentation-only preference without touching audio workers,
    /// provider connections, the session identity or open recording files.
    pub async fn set_interface_language(
        &self,
        language: String,
        revision: Option<u64>,
    ) -> Result<(crate::config::InterfaceConfig, u64)> {
        let mut state = self.state.lock().await;
        if revision.is_some_and(|expected| expected != state.config_revision) {
            return Err(ConfigurationChanged.into());
        }
        let interface = crate::config::InterfaceConfig { language };
        interface.validate()?;
        if state.config.interface.language != interface.language {
            let mut config = state.config.clone();
            config.interface = interface.clone();
            config.save(&self.path)?;
            state.config = config;
            cancel_pending_start(&mut state);
            state.config_revision = state.config_revision.wrapping_add(1);
        }
        Ok((interface, state.config_revision))
    }
    pub async fn set_config_if_revision(
        &self,
        mut config: AppConfig,
        revision: Option<u64>,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        if revision.is_some_and(|expected| expected != state.config_revision) {
            return Err(ConfigurationChanged.into());
        }
        ensure!(
            state.running.is_none(),
            "Encerre a sessão antes de alterar a configuração"
        );
        // Agent settings have their own live revision and API. An older main
        // settings form must never overwrite a newer agent configuration.
        config.agent = state.config.agent.clone();
        config.validate()?;
        stop_routing(&mut state).await?;
        if let Err(error) = config.save(&self.path) {
            maintain_routing(&mut state).await;
            return Err(error);
        }
        state.history.configure(&config.history);
        state.config = config;
        cancel_pending_start(&mut state);
        self.local_runtime.reconcile(&state.config);
        state.config_revision = state.config_revision.wrapping_add(1);
        state.last.last_error = None;
        state.routing_retry_at = Instant::now();
        maintain_routing(&mut state).await;
        Ok(())
    }
    /// Change the physical endpoint behind one virtual route without changing the virtual cable.
    pub async fn select_physical_device(
        &self,
        direction: DeviceDirection,
        id: String,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        let devices = audio::devices().await?;
        let route_was_configured = match direction {
            DeviceDirection::Input => crate::config::route_configured(&state.config.microphone),
            DeviceDirection::Output => crate::config::route_configured(&state.config.speaker),
        };
        let mut config = state.config.clone();
        select_physical(&mut config, direction, &id, &devices)?;
        config.save(&self.path)?;
        state.config = config;
        cancel_pending_start(&mut state);
        state.config_revision = state.config_revision.wrapping_add(1);
        state.last.last_error = None;
        if let Some(running) = &state.running {
            let sender = match direction {
                DeviceDirection::Input => &running.physical_input,
                DeviceDirection::Output => &running.physical_output,
            };
            // Even an initially unconfigured direction waits for this selection
            // without replacing the other route or the session writers.
            let _ = sender.send_replace(id);
        } else if !route_was_configured {
            // Configuring the second physical device must also start its bridge,
            // even if the other direction was already routing original audio.
            stop_routing(&mut state).await?;
            state.routing_retry_at = Instant::now();
            maintain_routing(&mut state).await;
        } else if let Some(routing) = &state.routing {
            let sender = match direction {
                DeviceDirection::Input => &routing.input,
                DeviceDirection::Output => &routing.output,
            };
            sender.send_replace(id);
        } else {
            state.routing_retry_at = Instant::now();
            maintain_routing(&mut state).await;
        }
        Ok(())
    }
    pub async fn start(&self) -> Result<()> {
        self.start_named(None).await
    }
    pub async fn start_named(&self, name: Option<String>) -> Result<()> {
        self.start_named_if_revision(name, None).await
    }
    pub async fn start_named_if_revision(
        &self,
        name: Option<String>,
        revision: Option<u64>,
    ) -> Result<()> {
        self.start_with_history(name, 0, revision).await
    }
    pub async fn start_with_history(
        &self,
        name: Option<String>,
        history_seconds: u32,
        revision: Option<u64>,
    ) -> Result<()> {
        self.commands.start()?;
        self.start_command_notifications();
        let mut state = self.state.lock().await;
        let language = crate::i18n::resolve_language(&state.config.interface.language);
        let session =
            crate::session::SessionIdentity::new_with_language(name.as_deref(), &language)?;
        reap(&mut state).await;
        if revision.is_some_and(|expected| expected != state.config_revision) {
            return Err(ConfigurationChanged.into());
        }
        ensure!(state.running.is_none(), "Uma sessão já está em execução");
        if state
            .pending_start
            .as_ref()
            .is_some_and(|(_, token)| token.is_cancelled())
        {
            state.pending_start = None;
        }
        ensure!(
            state.pending_start.is_none(),
            "Uma sessão está sendo preparada"
        );
        state.config.validate_for_start()?;
        history::validate_request(&state.config, history_seconds)?;
        let configured = state.config.clone();
        let config_revision = state.config_revision;
        let preparing = self.monitor_cancel.child_token();
        let prepare_guard = preparing.clone().drop_guard();
        state.next_start_id = state.next_start_id.wrapping_add(1);
        let start_id = state.next_start_id;
        state.pending_start = Some((start_id, preparing.clone()));
        // Model installation/loading must not block settings, Stop, status, or
        // original routing. No files or audio workers exist for this session yet.
        drop(state);
        let resolved = self
            .local_runtime
            .resolve(&configured, preparing.clone())
            .await;
        let mut state = self.state.lock().await;
        if state
            .pending_start
            .as_ref()
            .is_some_and(|(id, _)| *id == start_id)
        {
            state.pending_start = None;
        }
        ensure!(!preparing.is_cancelled(), "Preparação da sessão cancelada");
        if state.config_revision != config_revision {
            return Err(ConfigurationChanged.into());
        }
        ensure!(state.running.is_none(), "Uma sessão já está em execução");
        let (cfg, local_runtime_lease) = resolved?;
        drop(prepare_guard);
        let devices = audio::devices().await?;
        for (name, route, transcribe, record) in [
            (
                "microfone",
                &cfg.microphone,
                cfg.transcription.microphone,
                cfg.recording.microphone,
            ),
            (
                "saída",
                &cfg.speaker,
                cfg.transcription.speaker,
                cfg.recording.speaker,
            ),
        ] {
            if !(route.enabled
                || (cfg.transcription.enabled && transcribe)
                || (cfg.recording.enabled && record))
            {
                continue;
            }
            ensure!(
                devices
                    .iter()
                    .any(|d| d.id == route.capture_device && d.direction == DeviceDirection::Input),
                "Captura do fluxo {name} não encontrada: {}. Atualize a lista de dispositivos",
                route.capture_device
            );
            ensure!(
                devices.iter().any(
                    |d| d.id == route.playback_device && d.direction == DeviceDirection::Output
                ),
                "Reprodução do fluxo {name} não encontrada: {}. Atualize a lista de dispositivos",
                route.playback_device
            );
        }
        // Stop the old capture before taking a single snapshot. The new routes
        // start strictly after this boundary, so no original frame is saved twice.
        stop_routing(&mut state).await?;
        let now = Instant::now();
        let mut recent = state.history.snapshot(history_seconds, now);
        history::select_sources(&mut recent, &cfg, now);
        if history_seconds > 0 && recent.frames.is_empty() {
            maintain_routing(&mut state).await;
            bail!("Nenhum áudio recente disponível para as fontes selecionadas nesta sessão");
        }
        let origin = recent.origin;
        let files = create_session_files(&cfg, &session, origin).await;
        let SessionFiles { transcript, audio } = match files {
            Ok(files) => files,
            Err(error) => {
                maintain_routing(&mut state).await;
                return Err(error);
            }
        };
        let history_included_secs = recent.included_secs;
        let history_transcription_pending = Arc::new(AtomicBool::new(
            cfg.transcription.enabled && !recent.frames.is_empty(),
        ));
        let (physical_input, input_changes) = watch::channel(cfg.microphone.capture_device.clone());
        let (physical_output, output_changes) = watch::channel(cfg.speaker.playback_device.clone());
        let cancel = CancellationToken::new();
        let microphone = Arc::new(RouteMetrics::with_commands(&self.commands));
        let speaker = Arc::new(RouteMetrics::default());
        microphone.state("waiting_for_app");
        speaker.state("waiting_for_app");
        let task = tokio::spawn(run_session(
            cfg,
            cancel.clone(),
            microphone.clone(),
            speaker.clone(),
            SessionIo {
                transcript,
                audio,
                input_changes,
                output_changes,
                history: state.history.clone(),
                recent,
                session_origin: origin,
                history_transcription_pending: history_transcription_pending.clone(),
            },
        ));
        state.running = Some(Running {
            _local_runtime: local_runtime_lease,
            _cancel_on_drop: cancel.clone().drop_guard(),
            session,
            cancel,
            task,
            microphone,
            speaker,
            physical_input,
            physical_output,
            history_included_secs,
            history_transcription_pending,
        });
        state.last = stopped_status();
        state.routing_error = None;
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        cancel_pending_start(&mut state);
        if let Some(mut running) = state.running.take() {
            running.cancel.cancel();
            let result = tokio::time::timeout(Duration::from_secs(5), &mut running.task).await;
            let mut status = running.snapshot();
            status.running = false;
            status.routing_active = false;
            status.microphone.state = "stopped".into();
            status.speaker.state = "stopped".into();
            match result {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => status.last_error = Some(format!("{error:#}")),
                Ok(Err(_)) => {
                    status.last_error =
                        Some("A tarefa de áudio foi interrompida inesperadamente".into())
                }
                Err(_) => {
                    running.task.abort();
                    let _ = running.task.await;
                    status.last_error =
                        Some("Tempo limite ao parar o áudio; verifique os dispositivos".into());
                }
            }
            state.last = status;
        }
        maintain_routing(&mut state).await;
        Ok(())
    }
}

fn cancel_pending_start(state: &mut State) {
    if let Some((_, token)) = state.pending_start.take() {
        token.cancel();
    }
}

fn select_physical(
    config: &mut AppConfig,
    direction: DeviceDirection,
    id: &str,
    devices: &[audio::Device],
) -> Result<()> {
    ensure!(
        devices
            .iter()
            .any(|device| device.id == id && device.direction == direction && !device.is_virtual),
        "Dispositivo físico indisponível; atualize a lista na bandeja"
    );
    match direction {
        DeviceDirection::Input => config.microphone.capture_device = id.into(),
        DeviceDirection::Output => config.speaker.playback_device = id.into(),
    }
    Ok(())
}

fn stopped_status() -> EngineStatus {
    EngineStatus {
        microphone: RouteStatus {
            state: "stopped".into(),
            ..Default::default()
        },
        speaker: RouteStatus {
            state: "stopped".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
async fn reap(state: &mut State) {
    if state.running.as_ref().is_some_and(|r| r.task.is_finished()) {
        let running = state.running.take().expect("checked above");
        let mut status = running.snapshot();
        running.cancel.cancel();
        if let Err(error) = running
            .task
            .await
            .unwrap_or_else(|_| Err(anyhow!("Tarefa de áudio encerrada inesperadamente")))
        {
            status.last_error = Some(format!("{error:#}"));
        }
        status.running = false;
        status.routing_active = false;
        state.last = status;
    }
}

struct SessionFiles {
    transcript: Option<TranscriptWriter>,
    audio: Option<SessionAudioRecorder>,
}

async fn create_session_files(
    config: &AppConfig,
    session: &crate::session::SessionIdentity,
    origin: Instant,
) -> Result<SessionFiles> {
    let base = crate::storage::resolve_base(&config.files.base_path)?;
    // Resolve only enabled destinations, and resolve both before writing either
    // file. Legacy configurations can have empty folders for disabled features.
    let mut transcription = config.transcription.clone();
    if transcription.enabled {
        transcription.directory =
            crate::storage::resolve_directory(&base, &transcription.directory)?
                .to_str()
                .context("A pasta de transcrições precisa ser representável em UTF-8")?
                .to_owned();
    }
    let recording_directory = config
        .recording
        .enabled
        .then(|| crate::storage::resolve_directory(&base, &config.recording.directory))
        .transpose()?;
    let stem = session.file_stem(&config.files.name_pattern)?;
    let transcript = if transcription.enabled {
        Some(
            TranscriptWriter::create_merged(&transcription, &stem, &session.id, &session.name)
                .await?,
        )
    } else {
        None
    };
    let audio = if let Some(directory) = recording_directory {
        Some(
            SessionAudioRecorder::create(
                &directory,
                &stem,
                origin,
                config.recording.microphone && crate::config::route_configured(&config.microphone),
                config.recording.speaker && crate::config::route_configured(&config.speaker),
            )
            .await?,
        )
    } else {
        None
    };
    Ok(SessionFiles { transcript, audio })
}

struct SessionIo {
    transcript: Option<TranscriptWriter>,
    audio: Option<SessionAudioRecorder>,
    input_changes: watch::Receiver<String>,
    output_changes: watch::Receiver<String>,
    history: Arc<crate::history::HistoryBuffer>,
    recent: crate::history::HistorySnapshot,
    session_origin: Instant,
    history_transcription_pending: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TranscriptSink {
    sender: mpsc::Sender<TranscriptRecord>,
    origin: TranscriptOrigin,
}

#[derive(Clone)]
struct RouteIo {
    transcript: Option<TranscriptSink>,
    audio: Option<mpsc::Sender<AudioRecord>>,
    origin: TranscriptOrigin,
    capture_changes: watch::Receiver<String>,
    playback_changes: watch::Receiver<String>,
    history: Arc<crate::history::HistoryBuffer>,
    session_origin: Instant,
}

async fn run_session(
    cfg: AppConfig,
    cancel: CancellationToken,
    mic: Arc<RouteMetrics>,
    speaker: Arc<RouteMetrics>,
    io: SessionIo,
) -> Result<()> {
    let mut jobs = JoinSet::new();
    let transcript_tx = if let Some(writer) = io.transcript {
        let (tx, rx) = mpsc::channel(128);
        let recent = io.recent.clone();
        let config = cfg.clone();
        let history_cancel = cancel.child_token();
        let pending = io.history_transcription_pending.clone();
        jobs.spawn(async move {
            history::write_transcript(writer, rx, config, recent, history_cancel, pending)
                .await
                .context("Transcrição consolidada")
        });
        Some(tx)
    } else {
        None
    };
    let audio_tx = if let Some(writer) = io.audio {
        let (tx, rx) = mpsc::channel(256);
        let frames = io.recent.frames.clone();
        jobs.spawn(async move {
            writer
                .run_with_history(
                    rx,
                    frames.into_iter().map(|frame| AudioRecord {
                        lane: frame.lane,
                        samples: frame.samples().to_vec(),
                        captured_at: frame.captured_at,
                    }),
                )
                .await
                .context("Gravação do áudio original misturado")
        });
        Some(tx)
    } else {
        None
    };
    // Only the prefix workers retain this snapshot. Release the supervisor's
    // references now so old PCM is freed as soon as replay has completed.
    drop(io.recent);
    // Keep the unchanged virtual endpoints' watch senders alive throughout the session.
    let (_mic_virtual_tx, mic_virtual_rx) = watch::channel(cfg.microphone.playback_device.clone());
    let (_speaker_virtual_tx, speaker_virtual_rx) =
        watch::channel(cfg.speaker.capture_device.clone());
    let usage = audio::activity::monitor(
        mic_virtual_rx.clone(),
        speaker_virtual_rx.clone(),
        cancel.child_token(),
    );
    for (
        name,
        route,
        metrics,
        origin,
        capture_changes,
        playback_changes,
        transcribe,
        record_audio,
    ) in [
        (
            "microfone",
            cfg.microphone.clone(),
            mic,
            TranscriptOrigin::Microphone,
            io.input_changes,
            mic_virtual_rx,
            cfg.transcription.microphone,
            cfg.recording.microphone,
        ),
        (
            "saída",
            cfg.speaker.clone(),
            speaker,
            TranscriptOrigin::Speaker,
            speaker_virtual_rx,
            io.output_changes,
            cfg.transcription.speaker,
            cfg.recording.speaker,
        ),
    ] {
        {
            let transcript = if transcribe {
                transcript_tx
                    .clone()
                    .map(|sender| TranscriptSink { sender, origin })
            } else {
                None
            };
            let audio = if record_audio { audio_tx.clone() } else { None };
            jobs.spawn(run_selected_route(
                name,
                cfg.clone(),
                route,
                metrics,
                cancel.child_token(),
                usage.clone(),
                RouteIo {
                    transcript,
                    audio,
                    origin,
                    capture_changes,
                    playback_changes,
                    history: io.history.clone(),
                    session_origin: io.session_origin,
                },
            ));
        }
    }
    // Workers finish only after every route has released its sender. This lets
    // disk writers drain and finalize their files after audio/network shutdown.
    drop(transcript_tx);
    drop(audio_tx);
    let mut result = tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        result = jobs.join_next() => match result {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(anyhow!("Falha inesperada em áudio ou gravação")),
            None => Ok(()),
        }
    };
    cancel.cancel();
    while let Some(completed) = jobs.join_next().await {
        let completed = completed
            .context("Fluxo ou gravação encerrado inesperadamente")
            .and_then(|r| r);
        if completed.is_err() && result.is_ok() {
            result = completed;
        }
    }
    result
}

/// Keep an unconfigured direction ready for a physical selection without
/// opening an empty/default OS device or replacing the other session workers.
async fn wait_for_devices(
    capture: &mut watch::Receiver<String>,
    playback: &mut watch::Receiver<String>,
    cancel: &CancellationToken,
) -> bool {
    let (mut capture_open, mut playback_open) = (true, true);
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        if !capture.borrow().is_empty() && !playback.borrow().is_empty() {
            return true;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            changed = capture.changed(), if capture_open => { capture_open = changed.is_ok(); },
            changed = playback.changed(), if playback_open => { playback_open = changed.is_ok(); },
        }
    }
}

async fn run_selected_route(
    name: &'static str,
    cfg: AppConfig,
    route: RouteConfig,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    usage: watch::Receiver<audio::activity::EndpointUse>,
    io: RouteIo,
) -> Result<()> {
    let origin = io.origin;
    activity::while_selected(usage, origin, metrics.clone(), cancel, |active_cancel| {
        let cfg = cfg.clone();
        let route = route.clone();
        let metrics = metrics.clone();
        let io = io.clone();
        async move {
            // The outer RouteIo keeps both writers alive across activations.
            // Each activation gets fresh provider connections and PCM/event queues,
            // so delayed translation or ASR from a previous turn cannot escape.
            let transcript = io.transcript.clone();
            let result = run_route(name, cfg, route, metrics, active_cancel, io).await;
            record(&transcript, TranscriptRecord::Gap)?;
            result
        }
    })
    .await
}

async fn run_route(
    name: &'static str,
    cfg: AppConfig,
    route: RouteConfig,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    io: RouteIo,
) -> Result<()> {
    let RouteIo {
        transcript: transcript_tx,
        audio: audio_tx,
        origin,
        mut capture_changes,
        mut playback_changes,
        history,
        session_origin,
    } = io;
    if capture_changes.borrow().is_empty() || playback_changes.borrow().is_empty() {
        metrics.state("unconfigured");
        if !wait_for_devices(&mut capture_changes, &mut playback_changes, &cancel).await {
            return Ok(());
        }
    }
    let translating = route.enabled;
    let transcribing = transcript_tx.is_some();
    let needs_provider = translating || transcribing;
    let frame_ms = cfg.capture_frame_ms(&route);
    let capture_rate = if translating { INPUT_RATE } else { 48_000 };
    let playback_rate = if translating {
        OUTPUT_RATE
    } else {
        capture_rate
    };
    let playback_queue_ms = if translating {
        cfg.audio.playback_queue_ms
    } else {
        80
    };
    let (captured_tx, mut captured_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).max(1) as usize);
    let (input_tx, input_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / cfg.capture_frame_ms(&route)).max(1) as usize);
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let (stt_input_tx, stt_input_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).max(1) as usize);
    let (stt_events_tx, mut stt_events_rx) = mpsc::channel(16);
    let (play_tx, play_rx) = mpsc::channel((playback_queue_ms / OUTPUT_FRAME_MS).max(1) as usize);
    let mut jobs = JoinSet::new();
    let capture_options = AudioOptions {
        sample_rate: capture_rate,
        frame_ms,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: cfg.audio.capture_queue_ms,
    };
    let playback_options = AudioOptions {
        sample_rate: playback_rate,
        frame_ms: OUTPUT_FRAME_MS,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: playback_queue_ms,
    };
    let device = route.capture_device.clone();
    let capture_cancel = cancel.clone();
    let capture_stats = metrics.audio.clone();
    jobs.spawn(async move {
        audio::switching::capture(
            &device,
            capture_options,
            captured_tx,
            capture_changes,
            capture_cancel,
            capture_stats,
        )
        .await
        .context("Captura")
    });
    let device = route.playback_device.clone();
    let playback_cancel = cancel.clone();
    let playback_stats = metrics.audio.clone();
    jobs.spawn(async move {
        audio::switching::playback(
            &device,
            playback_options,
            play_rx,
            playback_changes,
            playback_cancel,
            playback_stats,
        )
        .await
        .context("Reprodução")
    });
    if translating {
        let provider_cancel = cancel.clone();
        let cloud = cfg.profile(&route.provider).clone();
        let native_voice = if route.provider == "local" {
            if route.voice.engine == "native" && !route.voice.voice_id.is_empty() {
                route.voice.voice_id.clone()
            } else {
                cfg.providers.local.piper_voice.clone()
            }
        } else if route.voice.engine == "native" && !route.voice.voice_id.is_empty() {
            route.voice.voice_id.clone()
        } else {
            cloud.voice.clone()
        };
        let session_config = SessionConfig {
            model: cloud.model.clone(),
            api_key_env: cloud.api_key_env.clone(),
            voice: native_voice,
            source_language: route.source_language.clone(),
            target_language: route.target_language.clone(),
            prompt: route.prompt.clone(),
            vad_silence_ms: cfg.audio.quality.vad_silence_ms(),
            connect_timeout_secs: cloud.connect_timeout_secs,
            max_reconnect_attempts: cloud.max_reconnect_attempts,
            input_transcription: false,
            output_transcription: translating && route.voice.engine != "native",
        };
        let provider_kind = route.provider.clone();
        let local_config = cfg.providers.local.clone();
        let synthesis = if translating && route.voice.engine != "native" {
            let voice_provider = cfg.profile(&route.voice.engine);
            Some(crate::voices::SynthesisConfig {
                provider: route.voice.engine.clone(),
                model: voice_provider.tts_model.clone(),
                api_key_env: voice_provider.api_key_env.clone(),
                voice_id: route.voice.voice_id.clone(),
                style: route.voice.style.clone(),
                language: route.target_language.clone(),
            })
        } else {
            None
        };
        let chunk_ms = route.voice.chunk_ms;
        let queue_ms = cfg.audio.playback_queue_ms;
        jobs.spawn(async move {
            let provider = provider::create_route_provider(
                &provider_kind,
                &cloud,
                &local_config,
                synthesis.is_none(),
            )?;
            if let Some(synthesis) = synthesis {
                crate::revoice::run(
                    provider,
                    session_config,
                    synthesis,
                    chunk_ms,
                    queue_ms,
                    input_rx,
                    events_tx,
                    provider_cancel,
                )
                .await
                .context("Tradução + síntese")
            } else {
                provider
                    .run(session_config, input_rx, events_tx, provider_cancel)
                    .await
                    .context("Provider")
            }
        });
    }
    if transcribing {
        let recognition = match origin {
            TranscriptOrigin::Microphone => cfg.transcription.microphone_recognition.clone(),
            TranscriptOrigin::Speaker => cfg.transcription.speaker_recognition.clone(),
        };
        let profiles = cfg.transcription.providers.clone();
        let stt_cancel = cancel.clone();
        jobs.spawn(async move {
            let recognizer = provider::stt::create(&recognition, &profiles)?;
            let session_config = provider::stt::session_config(&recognition, &profiles)?;
            recognizer
                .run(session_config, stt_input_rx, stt_events_tx, stt_cancel)
                .await
                .context("Transcrição STT")
        });
    }
    if !translating {
        metrics.state(if needs_provider {
            "connecting"
        } else {
            "passthrough"
        });
    }
    let mut connected = false;
    let mut stt_connected = false;
    let mut stt_offset_ms = None;
    let mut tap_resampler = audio::resample::Resampler::new(capture_rate, INPUT_RATE);
    let mut original_float = Vec::with_capacity(4800);
    let mut tap_float = Vec::with_capacity(1600);
    let result: Result<()> = async {
        loop {
            if cancel.is_cancelled() { break Ok(()); }
            tokio::select! {
                _ = cancel.cancelled() => break Ok(()),
                completed = jobs.join_next() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    match completed {
                        Some(Ok(Err(error))) => break Err(error),
                        _ => bail!("Um componente de áudio encerrou inesperadamente"),
                    }
                }
                event = events_rx.recv(), if translating => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(event) = event else { bail!("Provider encerrou o canal de áudio"); };
                    match event {
                        ProviderEvent::Connected => { connected = true; metrics.state("running"); }
                        ProviderEvent::Reconnecting { .. } => {
                            connected = false;
                            metrics.state("reconnecting");
                            metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            interrupt(&metrics, &play_tx);
                        }
                        ProviderEvent::Interrupted => { interrupt(&metrics, &play_tx); }
                        ProviderEvent::Audio { mut samples, sample_rate } => {
                            ensure!(sample_rate == OUTPUT_RATE, "Provider devolveu taxa de áudio não suportada: {sample_rate}");
                            ensure!(samples.len() <= OUTPUT_RATE as usize, "Bloco de áudio do provider excede 1 segundo");
                            apply_gain(&mut samples, route.gain);
                            metrics.output_level.store(rms(&samples).to_bits(), Ordering::Relaxed);
                            metrics.translated_samples.fetch_add(samples.len() as u64, Ordering::Relaxed);
                            let generation = metrics.audio.playback_generation.load(Ordering::Acquire);
                            for chunk in samples.chunks(OUTPUT_FRAME_SAMPLES) {
                                if play_tx.try_send(PlaybackCommand::Audio { samples: chunk.to_vec(), generation }).is_err() {
                                    metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed);
                                    bail!("Fila de reprodução cheia. O fluxo foi parado para evitar atraso acumulado; aumente playback_queue_ms ou verifique a velocidade do dispositivo/modelo");
                                }
                            }
                        }
                        // STS may emit transcripts for its own synthesis protocol. Only
                        // the independently selected STT stream owns the saved original.
                        ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete => {}
                    }
                }
                event = stt_events_rx.recv(), if transcribing => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(event) = event else { bail!("Provider STT encerrou o canal de transcrição"); };
                    match &event {
                        ProviderEvent::Connected => {
                            stt_connected = true;
                            if !translating { metrics.state("transcribing"); }
                        }
                        ProviderEvent::Reconnecting { .. } => {
                            stt_connected = false;
                            stt_offset_ms = None;
                            metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            if !translating { metrics.state("reconnecting"); }
                        }
                        _ => {}
                    }
                    record_recognition_event_at(event, &transcript_tx, &metrics, stt_offset_ms.unwrap_or(0))?;
                }
                frame = captured_rx.recv() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(mut frame) = frame else { bail!("Captura de áudio foi encerrada"); };
                    let input_level = rms(&frame.samples);
                    metrics.input_level.store(input_level.to_bits(), Ordering::Relaxed);
                    ensure!(frame.sample_rate == capture_rate, "Formato de captura incompatível");
                    if !translating {
                        let taps = if needs_provider || audio_tx.is_some() || cfg.history.enabled {
                            original_float.clear(); tap_float.clear();
                            original_float.extend(frame.samples.iter().map(|&v| f32::from(v) / 32768.0));
                            tap_resampler.process(&original_float, &mut tap_float);
                            tap_float.iter().map(|v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).collect()
                        } else { Vec::new() };
                        metrics.output_level.store(input_level.to_bits(), Ordering::Relaxed);
                        let generation = metrics.audio.playback_generation.load(Ordering::Acquire);
                        if frame.captured_at.elapsed() <= Duration::from_millis(100) {
                            if play_tx.try_send(PlaybackCommand::Audio { samples: std::mem::take(&mut frame.samples), generation }).is_err() {
                                metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed);
                                interrupt(&metrics, &play_tx);
                            }
                        } else { metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed); }
                        frame.samples = taps;
                    }
                    history.push(match origin { TranscriptOrigin::Microphone => RecordingLane::Microphone, TranscriptOrigin::Speaker => RecordingLane::Speaker }, &frame.samples, frame.captured_at);
                    if let Some(sender) = &audio_tx {
                        sender.try_send(AudioRecord { lane: match origin { TranscriptOrigin::Microphone => RecordingLane::Microphone, TranscriptOrigin::Speaker => RecordingLane::Speaker }, samples: frame.samples.clone(), captured_at: frame.captured_at })
                            .map_err(|_| anyhow!("Gravação de áudio original indisponível ou lenta; sessão parada para evitar perda silenciosa"))?;
                    }
                    if needs_provider {
                        if frame.captured_at.elapsed() > Duration::from_millis(u64::from(cfg.audio.max_capture_age_ms)) {
                            metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed);
                        } else {
                            if transcribing && stt_connected && stt_offset_ms.is_none() {
                                let start = frame.captured_at.checked_sub(Duration::from_secs_f64(frame.samples.len() as f64 / f64::from(INPUT_RATE))).unwrap_or(frame.captured_at);
                                stt_offset_ms = Some(start.saturating_duration_since(session_origin).as_millis().min(u128::from(u64::MAX)) as u64);
                            }
                            fanout_original_audio(frame.samples,
                                (translating && connected).then_some(&input_tx),
                                (transcribing && stt_connected).then_some(&stt_input_tx), &metrics);
                        }
                    }
                }
            }
        }
    }.await;
    cancel.cancel();
    interrupt(&metrics, &play_tx);
    drop(input_tx);
    drop(stt_input_tx);
    drop(play_tx);
    drop(audio_tx);
    let mut cleanup_result = Ok(());
    while let Some(completed) = jobs.join_next().await {
        let completed = completed
            .context("Tarefa de áudio/transcrição interrompida")
            .and_then(|r| r);
        if completed.is_err() && cleanup_result.is_ok() {
            cleanup_result = completed;
        }
    }
    // Save finals already delivered before cancellation. Never wait for a model
    // to finish another turn or replay a result into a later device activation.
    while let Ok(event) = stt_events_rx.try_recv() {
        if let Err(error) =
            record_recognition_event_at(event, &transcript_tx, &metrics, stt_offset_ms.unwrap_or(0))
            && cleanup_result.is_ok()
        {
            cleanup_result = Err(error);
        }
    }
    drop(transcript_tx);
    metrics.input_level.store(0, Ordering::Relaxed);
    metrics.output_level.store(0, Ordering::Relaxed);
    metrics.state(if result.is_ok() && cleanup_result.is_ok() {
        "stopped"
    } else {
        "error"
    });
    result
        .and(cleanup_result)
        .with_context(|| format!("Fluxo {name}"))
}

/// Independent bounded queues: a slow recognizer cannot hold translation or
/// physical playback. Both receive the same original PCM, before voice/gain.
fn fanout_original_audio(
    samples: Vec<i16>,
    translation: Option<&mpsc::Sender<Vec<i16>>>,
    recognition: Option<&mpsc::Sender<Vec<i16>>>,
    metrics: &RouteMetrics,
) {
    let send = |sender: &mpsc::Sender<Vec<i16>>, samples| {
        if sender.try_send(samples).is_err() {
            metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed);
        }
    };
    match (translation, recognition) {
        (Some(translation), Some(recognition)) => {
            send(translation, samples.clone());
            send(recognition, samples);
        }
        (Some(sender), None) | (None, Some(sender)) => send(sender, samples),
        (None, None) => {}
    }
}

#[cfg(test)]
fn record_recognition_event(
    event: ProviderEvent,
    transcript: &Option<TranscriptSink>,
    metrics: &RouteMetrics,
) -> Result<()> {
    record_recognition_event_at(event, transcript, metrics, 0)
}
fn record_recognition_event_at(
    event: ProviderEvent,
    transcript: &Option<TranscriptSink>,
    metrics: &RouteMetrics,
    offset_ms: u64,
) -> Result<()> {
    match event {
        ProviderEvent::Transcript {
            input: true,
            text,
            mut metadata,
        } => {
            for value in [
                &mut metadata.start_ms,
                &mut metadata.end_ms,
                &mut metadata.alignment_ms,
            ]
            .into_iter()
            .flatten()
            {
                *value = value.saturating_add(offset_ms);
            }
            metrics
                .view
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last_input_transcript = Some(text.chars().take(2048).collect());
            record(
                transcript,
                TranscriptRecord::Text {
                    input: true,
                    text,
                    metadata,
                    received_at: chrono::Utc::now().to_rfc3339(),
                },
            )
        }
        ProviderEvent::TurnComplete => record(transcript, TranscriptRecord::TurnComplete),
        ProviderEvent::Reconnecting { .. } | ProviderEvent::Interrupted => {
            record(transcript, TranscriptRecord::Gap)
        }
        // Recognition has no access to playback. Defensive against a broken
        // endpoint returning generated audio or translated text.
        _ => Ok(()),
    }
}

fn record(sender: &Option<TranscriptSink>, record: TranscriptRecord) -> Result<()> {
    if let Some(sender) = sender {
        sender.sender.try_send(TranscriptRecord::Routed { origin: sender.origin, record: Box::new(record) }).map_err(|_| anyhow!("Gravação de transcrição indisponível ou lenta; fluxo parado para evitar perda silenciosa"))?;
    }
    Ok(())
}
fn interrupt(metrics: &RouteMetrics, playback: &mpsc::Sender<PlaybackCommand>) {
    metrics
        .audio
        .playback_generation
        .fetch_add(1, Ordering::AcqRel);
    let _ = playback.try_send(PlaybackCommand::Flush);
}
fn apply_gain(samples: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() > f32::EPSILON {
        for sample in samples {
            *sample = (f32::from(*sample) * gain)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
    }
}
fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples
        .iter()
        .map(|&v| {
            let v = f64::from(v) / 32768.0;
            v * v
        })
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt() as f32
}

#[cfg(test)]
#[path = "engine/stt_tests.rs"]
mod stt_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_and_saved_configuration_cancel_preparation_without_starting_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let controller =
            Controller::new(AppConfig::default(), directory.path().join("config.toml")).unwrap();
        let stop_token = CancellationToken::new();
        controller.state.lock().await.pending_start = Some((1, stop_token.clone()));
        controller.stop().await.unwrap();
        assert!(stop_token.is_cancelled());
        assert!(!controller.status().await.running);
        assert!(controller.state.lock().await.pending_start.is_none());

        let change_token = CancellationToken::new();
        controller.state.lock().await.pending_start = Some((2, change_token.clone()));
        let mut config = controller.config().await;
        config.local_runtime.threads = 2;
        controller.set_config(config).await.unwrap();
        assert!(change_token.is_cancelled());
        assert_eq!(controller.config().await.local_runtime.threads, 2);
        assert!(controller.state.lock().await.running.is_none());
        assert_eq!(
            directory.path().read_dir().unwrap().count(),
            1,
            "preparation must not open recording/transcript files"
        );
        controller.shutdown().await.unwrap();
    }

    #[test]
    fn file_preview_uses_only_an_explicit_absolute_base() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let controller =
            Controller::new(AppConfig::default(), elsewhere.path().join("config.toml")).unwrap();
        let paths = controller
            .file_paths(base.path().to_str().unwrap(), "text", "audio")
            .unwrap();
        assert_eq!(paths.base_path, base.path());
        assert_eq!(paths.recording_directory, base.path().join("audio"));
        assert!(controller.file_paths("archive", "text", "audio").is_err());
        assert!(elsewhere.path().read_dir().unwrap().next().is_none());
    }

    #[tokio::test]
    async fn session_files_write_real_txt_and_wav_under_base_and_absolute_override() {
        for absolute_override in [false, true] {
            for (transcribe, record) in [(true, false), (false, true), (true, true)] {
                let launch = tempfile::tempdir().unwrap();
                let external = tempfile::tempdir().unwrap();
                let base = launch.path().join("new/base/archive");
                let destination_base = if absolute_override {
                    external.path().join("new/external/archive")
                } else {
                    base.clone()
                };
                let text_directory = destination_base.join("text/original");
                let audio_directory = destination_base.join("audio/original");
                let mut cfg = AppConfig::default();
                cfg.files.base_path = base.to_str().unwrap().into();
                cfg.transcription.enabled = transcribe;
                cfg.transcription.directory = if absolute_override {
                    text_directory.to_str().unwrap().into()
                } else {
                    "text/original".into()
                };
                cfg.recording.enabled = record;
                cfg.recording.directory = if absolute_override {
                    audio_directory.to_str().unwrap().into()
                } else {
                    "audio/original".into()
                };
                cfg.microphone.capture_device = "test-mic".into();
                cfg.microphone.playback_device = "test-virtual-mic".into();
                cfg.speaker.capture_device = "test-virtual-speaker".into();
                cfg.speaker.playback_device = "test-speaker".into();
                let before = toml::to_string(&cfg).unwrap();
                let session = crate::session::SessionIdentity::new(Some("Storage test")).unwrap();
                let stem = session.file_stem(&cfg.files.name_pattern).unwrap();
                let origin = Instant::now() - Duration::from_secs(1);
                assert!(!base.exists() && !destination_base.exists());
                let files = create_session_files(&cfg, &session, origin).await.unwrap();
                assert_eq!(files.transcript.is_some(), transcribe);
                assert_eq!(files.audio.is_some(), record);
                if let Some(transcript) = files.transcript {
                    let (text_tx, text_rx) = mpsc::channel(1);
                    text_tx
                        .send(TranscriptRecord::Text {
                            input: true,
                            text: "original speech".into(),
                            metadata: crate::provider::TranscriptMetadata::default(),
                            received_at: "2026-09-29T12:00:00Z".into(),
                        })
                        .await
                        .unwrap();
                    drop(text_tx);
                    transcript.run(text_rx).await.unwrap();
                    let text =
                        tokio::fs::read_to_string(text_directory.join(format!("{stem}.txt")))
                            .await
                            .unwrap();
                    assert!(text.contains("original speech"));
                    assert_eq!(text_directory.read_dir().unwrap().count(), 1);
                } else {
                    assert!(!destination_base.join("text").exists());
                }
                if let Some(audio) = files.audio {
                    let expected_audio = audio_directory.join(format!("{stem}.wav"));
                    assert_eq!(audio.path(), expected_audio);
                    let (audio_tx, audio_rx) = mpsc::channel(1);
                    audio_tx
                        .send(AudioRecord {
                            lane: RecordingLane::Microphone,
                            samples: vec![1000; 320],
                            captured_at: origin + Duration::from_millis(20),
                        })
                        .await
                        .unwrap();
                    drop(audio_tx);
                    audio.run(audio_rx).await.unwrap();
                    let mut wav = hound::WavReader::open(expected_audio).unwrap();
                    assert_eq!(wav.spec().channels, 1);
                    assert!(wav.samples::<i16>().any(|sample| sample.unwrap() != 0));
                    assert_eq!(audio_directory.read_dir().unwrap().count(), 1);
                } else {
                    assert!(!destination_base.join("audio").exists());
                }
                assert_eq!(base.exists(), !absolute_override);
                assert_eq!(toml::to_string(&cfg).unwrap(), before);
                assert!(!launch.path().join("text").exists());
                assert!(!launch.path().join("audio").exists());
            }
        }
    }

    #[tokio::test]
    async fn session_files_report_blocked_directories_without_replacing_existing_data() {
        for transcribe in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let blocker = root.path().join("existing-file");
            let existing = b"Existing user data must remain unchanged";
            tokio::fs::write(&blocker, existing).await.unwrap();
            let mut cfg = AppConfig::default();
            cfg.files.base_path = root.path().to_str().unwrap().into();
            cfg.transcription.enabled = transcribe;
            cfg.transcription.directory = "existing-file/nested/text".into();
            cfg.recording.enabled = !transcribe;
            cfg.recording.directory = "existing-file/nested/audio".into();
            cfg.microphone.capture_device = "test-mic".into();
            cfg.microphone.playback_device = "test-virtual-mic".into();
            let session = crate::session::SessionIdentity::new(Some("Blocked folder")).unwrap();
            let error = create_session_files(&cfg, &session, Instant::now())
                .await
                .err()
                .expect("A file used as a parent directory must fail");
            assert!(error.downcast_ref::<std::io::Error>().is_some());
            assert_eq!(tokio::fs::read(&blocker).await.unwrap(), existing);
            assert_eq!(root.path().read_dir().unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn session_files_ignore_empty_disabled_destinations_and_resolve_before_writing() {
        let launch = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::default();
        cfg.files.base_path = launch.path().join("archive").to_str().unwrap().into();
        cfg.transcription.directory.clear();
        cfg.recording.directory.clear();
        let session = crate::session::SessionIdentity::new(None).unwrap();
        let files = create_session_files(&cfg, &session, Instant::now())
            .await
            .unwrap();
        assert!(files.transcript.is_none() && files.audio.is_none());
        assert!(!launch.path().join("archive").exists());

        cfg.transcription.enabled = true;
        cfg.transcription.directory = "text".into();
        cfg.recording.enabled = true;
        assert!(
            create_session_files(&cfg, &session, Instant::now())
                .await
                .is_err()
        );
        assert!(
            !launch.path().join("archive").exists(),
            "No TXT should be opened before validating the enabled WAV destination"
        );

        cfg.recording.enabled = false;
        let files = create_session_files(&cfg, &session, Instant::now())
            .await
            .unwrap();
        assert!(files.audio.is_none());
        let (sender, receiver) = mpsc::channel(1);
        drop(sender);
        files.transcript.unwrap().run(receiver).await.unwrap();
    }

    #[test]
    fn gain_saturates_instead_of_wrapping() {
        let mut pcm = [30000, -30000, 50];
        apply_gain(&mut pcm, 2.0);
        assert_eq!(pcm, [32767, -32768, 100]);
        assert_eq!(rms(&[]), 0.0);
    }
    #[tokio::test]
    async fn interrupt_invalidates_audio_even_with_full_playback_channel() {
        let metrics = RouteMetrics::default();
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(PlaybackCommand::Audio {
            samples: vec![1; 480],
            generation: 0,
        })
        .await
        .unwrap();
        interrupt(&metrics, &tx);
        let PlaybackCommand::Audio { generation, .. } = rx.recv().await.unwrap() else {
            panic!("expected queued audio")
        };
        assert_ne!(
            generation,
            metrics.audio.playback_generation.load(Ordering::Acquire)
        );
    }
    #[tokio::test]
    async fn an_unconfigured_direction_can_be_selected_during_another_session() {
        let (capture_tx, mut capture_rx) = watch::channel(String::new());
        let (playback_tx, mut playback_rx) = watch::channel("virtual-end".into());
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let worker = tokio::spawn(async move {
            wait_for_devices(&mut capture_rx, &mut playback_rx, &worker_cancel).await
        });
        tokio::task::yield_now().await;
        assert!(
            !worker.is_finished(),
            "Never open a default physical device while selection is empty"
        );
        capture_tx.send_replace("physical-mic".into());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), worker)
                .await
                .unwrap()
                .unwrap()
        );
        drop(playback_tx);
        let (_, mut empty) = watch::channel(String::new());
        let (_, mut closed) = watch::channel(String::new());
        let worker_cancel = cancel.clone();
        let worker =
            tokio::spawn(
                async move { wait_for_devices(&mut empty, &mut closed, &worker_cancel).await },
            );
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(
            !tokio::time::timeout(Duration::from_millis(100), worker)
                .await
                .unwrap()
                .unwrap()
        );
    }
    #[tokio::test]
    async fn saving_config_and_stopping_idle_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let controller =
            Controller::new(AppConfig::default(), dir.path().join("babel.toml")).unwrap();
        assert_eq!(controller.status().await.config_revision, 0);
        controller.stop().await.unwrap();
        controller.set_config(AppConfig::default()).await.unwrap();
        assert_eq!(controller.status().await.config_revision, 1);
        assert!(!controller.status().await.running);
    }
    #[tokio::test]
    async fn stale_panel_cannot_overwrite_or_start_a_newer_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let controller =
            Controller::new(AppConfig::default(), dir.path().join("babel.toml")).unwrap();
        let (mut config, revision) = controller.config_snapshot().await;
        config.microphone.target_language = "es-ES".into();
        controller
            .set_config_if_revision(config.clone(), Some(revision))
            .await
            .unwrap();
        config.microphone.target_language = "fr-FR".into();
        let error = controller
            .set_config_if_revision(config, Some(revision))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ConfigurationChanged>().is_some());
        let error = controller
            .start_named_if_revision(Some("Antiga".into()), Some(revision))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ConfigurationChanged>().is_some());
        assert_eq!(
            controller.config().await.microphone.target_language,
            "es-ES"
        );
        assert!(!controller.status().await.running);
    }
    #[tokio::test]
    async fn interface_change_preserves_live_session_and_rejects_stale_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let controller = Controller::new(AppConfig::default(), path.clone()).unwrap();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            worker_cancel.cancelled().await;
            Ok(())
        });
        let task_id = task.id();
        let metrics = Arc::new(RouteMetrics::default());
        let session =
            crate::session::SessionIdentity::new(Some("Minha sessão / unchanged")).unwrap();
        let session_id = session.id.clone();
        let (input, _) = watch::channel("my-mic".into());
        let (output, _) = watch::channel("my-speakers".into());
        controller.state.lock().await.running = Some(Running {
            _local_runtime: None,
            _cancel_on_drop: cancel.clone().drop_guard(),
            session,
            cancel: cancel.clone(),
            task,
            microphone: metrics.clone(),
            speaker: Arc::new(RouteMetrics::default()),
            physical_input: input,
            physical_output: output,
            history_included_secs: 0.0,
            history_transcription_pending: Arc::new(AtomicBool::new(false)),
        });
        let (interface, revision) = controller
            .set_interface_language("en".into(), Some(0))
            .await
            .unwrap();
        assert_eq!(interface.language, "en");
        assert_eq!(revision, 1);
        assert_eq!(AppConfig::load(&path).unwrap().interface.language, "en");
        let agent = crate::commands::AgentConfig {
            wake_name: "Atlas".into(),
            ..Default::default()
        };
        assert_eq!(controller.set_agent(agent, Some(0)).await.unwrap(), 1);
        assert_eq!(controller.config_snapshot().await.1, revision);
        assert_eq!(controller.agent_status().await.0.wake_name, "Atlas");
        {
            let state = controller.state.lock().await;
            let running = state.running.as_ref().unwrap();
            assert_eq!(running.task.id(), task_id);
            assert!(Arc::ptr_eq(&running.microphone, &metrics));
            assert_eq!(running.session.id, session_id);
            assert_eq!(running.session.name, "Minha sessão / unchanged");
            assert_eq!(*running.physical_input.borrow(), "my-mic");
            assert!(!running.cancel.is_cancelled());
        }
        let error = controller
            .set_interface_language("pt".into(), Some(0))
            .await
            .unwrap_err();
        assert!(error.is::<ConfigurationChanged>());
        assert!(
            controller
                .set_interface_language("xx".into(), Some(1))
                .await
                .is_err()
        );
        assert_eq!(controller.config().await.interface.language, "en");
        controller.shutdown().await.unwrap();
    }
    #[test]
    fn tray_device_selection_changes_only_physical_ends_and_rejects_virtual_devices() {
        let mut config = AppConfig::default();
        let devices = vec![
            audio::Device {
                id: "mic".into(),
                name: "Mic".into(),
                direction: DeviceDirection::Input,
                is_virtual: false,
            },
            audio::Device {
                id: "headphones".into(),
                name: "Fones".into(),
                direction: DeviceDirection::Output,
                is_virtual: false,
            },
            audio::Device {
                id: "virtual".into(),
                name: "Monitor".into(),
                direction: DeviceDirection::Input,
                is_virtual: true,
            },
        ];
        let mic_bus = config.microphone.playback_device.clone();
        let speaker_monitor = config.speaker.capture_device.clone();
        select_physical(&mut config, DeviceDirection::Input, "mic", &devices).unwrap();
        select_physical(&mut config, DeviceDirection::Output, "headphones", &devices).unwrap();
        assert_eq!(config.microphone.capture_device, "mic");
        assert_eq!(config.speaker.playback_device, "headphones");
        assert_eq!(config.microphone.playback_device, mic_bus);
        assert_eq!(config.speaker.capture_device, speaker_monitor);
        assert!(select_physical(&mut config, DeviceDirection::Input, "virtual", &devices).is_err());
        assert!(
            select_physical(&mut config, DeviceDirection::Input, "headphones", &devices).is_err()
        );
        assert!(
            select_physical(&mut config, DeviceDirection::Output, "missing", &devices).is_err()
        );
        assert_eq!(config.microphone.capture_device, "mic");
    }
}
