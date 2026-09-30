//! Local inference lifecycle. Only selected providers are prepared. Audio stays
//! in the engine's gated routes; this manager never opens an audio device.
mod assets;
mod piper;
mod process;

use crate::config::AppConfig;
use anyhow::{Context, Result, bail};
use assets::{Asset, Bundle, Catalog};
use process::ServiceProcess;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Serialize)]
pub struct RuntimeStatus {
    pub phase: String,
    pub message: Option<String>,
    pub download: Option<DownloadProgress>,
    pub services: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct DownloadProgress {
    pub name: String,
    pub received: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    directory: String,
    threads: u32,
    idle_unload_secs: u64,
    whisper: BTreeSet<String>,
    translation: Option<String>,
    voices: BTreeSet<String>,
    issues: Vec<String>,
}
fn automatic(endpoint: &str) -> bool {
    endpoint.is_empty() || endpoint == "auto"
}
impl Plan {
    /// Only enabled features load models into memory. Selection separately
    /// prepares their assets on disk, without launching inference processes.
    fn active(cfg: &AppConfig) -> Result<Self> {
        let mut active = cfg.clone();
        for route in [&mut active.microphone, &mut active.speaker] {
            if !route.enabled {
                route.provider = "gemini".into();
            }
        }
        if !active.transcription.enabled || !active.transcription.microphone {
            active.transcription.microphone_recognition.provider = "gemini".into();
        }
        if !active.transcription.enabled || !active.transcription.speaker {
            active.transcription.speaker_recognition.provider = "gemini".into();
        }
        let plan = Self::from_config(&active)?;
        if !plan.issues.is_empty() {
            bail!("{}", plan.issues.join("; "));
        }
        Ok(plan)
    }

    fn from_config(cfg: &AppConfig) -> Result<Self> {
        let mut plan = Self {
            directory: cfg.local_runtime.directory.clone(),
            threads: crate::execution::inference_threads(cfg.local_runtime.threads),
            idle_unload_secs: u64::from(cfg.local_runtime.idle_unload_secs),
            whisper: BTreeSet::new(),
            translation: None,
            voices: BTreeSet::new(),
            issues: Vec::new(),
        };
        let catalog = Catalog::read();
        let local = &cfg.providers.local;
        for (route, available) in [
            (&cfg.microphone, !cfg.microphone_uses_speaker()),
            (&cfg.speaker, true),
        ] {
            if !available || route.provider != "local" {
                continue;
            }
            if automatic(&local.whisper_endpoint) {
                plan.whisper.insert(local.whisper_model.clone());
            }
            if automatic(&local.ollama_endpoint) {
                plan.translation = Some(local.translation_model.clone());
            }
            if automatic(&local.piper_endpoint) {
                match catalog.voice("auto", &route.target_language) {
                    Ok(voice) => {
                        plan.voices.insert(voice);
                    }
                    Err(error) => plan.issues.push(error.to_string()),
                }
            }
        }
        let stt = &cfg.transcription;
        if ((!cfg.microphone_uses_speaker() && stt.microphone_recognition.provider == "whisper")
            || stt.speaker_recognition.provider == "whisper")
            && automatic(&stt.providers.whisper.endpoint)
        {
            plan.whisper.insert(stt.providers.whisper.model.clone());
        }
        plan.whisper.retain(|model| {
            if !catalog.whisper.contains_key(model) {
                plan.issues
                    .push(format!("Unknown bundled Whisper model: {model}"));
                false
            } else {
                true
            }
        });
        if let Some(model) = &plan.translation
            && !catalog.translation.contains_key(model)
        {
            plan.issues
                .push(format!("Unknown bundled translation model: {model}"));
            plan.translation = None;
        }
        Ok(plan)
    }
    fn empty(&self) -> bool {
        self.whisper.is_empty() && self.translation.is_none() && self.voices.is_empty()
    }
    fn satisfied_by(&self, endpoints: &Endpoints) -> bool {
        self.whisper
            .iter()
            .all(|model| endpoints.whisper.contains_key(model))
            && (self.translation.is_none() || endpoints.translation.is_some())
            && (self.voices.is_empty()
                || (endpoints.piper.is_some() && self.voices.is_subset(&endpoints.voices)))
    }
}
#[derive(Clone, Default)]
struct Endpoints {
    whisper: BTreeMap<String, String>,
    translation: Option<String>,
    piper: Option<String>,
    voices: BTreeSet<String>,
}
struct State {
    plan: Option<Plan>,
    inference: bool,
    leases: usize,
    idle_task: Option<JoinHandle<()>>,
    idle_epoch: u64,
    generation: u64,
    status: RuntimeStatus,
    endpoints: Endpoints,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
/// An active session owns this lease. Releasing the last one permits the
/// manager to unload inference after a short, configurable reuse window.
pub struct RuntimeLease {
    manager: Weak<RuntimeManager>,
    generation: u64,
}
impl std::fmt::Debug for RuntimeLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeLease")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl Drop for RuntimeLease {
    fn drop(&mut self) {
        if let Some(manager) = self.manager.upgrade() {
            manager.release(self.generation);
        }
    }
}

pub struct RuntimeManager {
    state: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
}
impl Drop for RuntimeManager {
    fn drop(&mut self) {
        let state = self.state.lock().expect("runtime state");
        state.cancel.cancel();
        if let Some(task) = &state.idle_task {
            task.abort();
        }
    }
}
impl RuntimeManager {
    pub fn new() -> Arc<Self> {
        let (changed, _) = watch::channel(0);
        Arc::new(Self {
            state: Arc::new(Mutex::new(State {
                plan: None,
                inference: false,
                leases: 0,
                idle_task: None,
                idle_epoch: 0,
                generation: 0,
                status: RuntimeStatus {
                    phase: "idle".into(),
                    ..Default::default()
                },
                endpoints: Endpoints::default(),
                cancel: CancellationToken::new(),
                task: None,
            })),
            changed,
        })
    }
    pub fn status(&self) -> RuntimeStatus {
        self.state.lock().expect("runtime state").status.clone()
    }
    pub fn reconcile(self: &Arc<Self>, cfg: &AppConfig) {
        self.reconcile_inner(Plan::from_config(cfg), false, true)
    }
    fn reconcile_inner(self: &Arc<Self>, desired: Result<Plan>, inference: bool, retry: bool) {
        let mut state = self.state.lock().expect("runtime state");
        if let Ok(plan) = &desired
            && state.plan.as_ref() == Some(plan)
            && state.inference == inference
            && !(retry && state.status.phase == "error")
        {
            return;
        }
        state.cancel.cancel();
        if let Some(task) = state.idle_task.take() {
            task.abort();
        }
        state.inference = inference;
        state.leases = 0;
        state.generation += 1;
        state.endpoints = Endpoints::default();
        state.cancel = CancellationToken::new();
        // The prior worker observes cancellation and drops only its own children.
        let previous = state.task.take();
        let plan = match desired {
            Ok(p) => p,
            Err(error) => {
                state.plan = None;
                state.status = RuntimeStatus {
                    phase: "error".into(),
                    message: Some(error.to_string()),
                    ..Default::default()
                };
                state.task = previous;
                self.changed.send_replace(state.generation);
                return;
            }
        };
        if plan.empty() {
            state.status = RuntimeStatus {
                phase: if plan.issues.is_empty() {
                    "idle"
                } else {
                    "error"
                }
                .into(),
                message: (!plan.issues.is_empty()).then(|| plan.issues.join("; ")),
                ..Default::default()
            };
            state.plan = Some(plan);
            state.task = previous;
            self.changed.send_replace(state.generation);
            return;
        }
        let Ok(_) = tokio::runtime::Handle::try_current() else {
            state.plan = None;
            state.task = previous;
            return;
        };
        let handle = match crate::execution::processing_handle() {
            Ok(handle) => handle,
            Err(error) => {
                state.plan = None;
                state.task = previous;
                state.status.phase = "error".into();
                state.status.message = Some(format!("{error:#}"));
                return;
            }
        };
        state.plan = Some(plan.clone());
        state.status = RuntimeStatus {
            phase: "preparing".into(),
            ..Default::default()
        };
        let generation = state.generation;
        let cancel = state.cancel.clone();
        let shared = self.state.clone();
        let changed = self.changed.clone();
        state.task=Some(handle.spawn(async move {
            // Retire the previous generation before loading its replacement;
            // model memory must not double with every configuration change.
            if let Some(mut previous) = previous
                && tokio::time::timeout(Duration::from_secs(5), &mut previous).await.is_err() {
                    previous.abort();
                    let _ = previous.await;
            }
            let update=|status:RuntimeStatus| {
                let mut state=shared.lock().expect("runtime state");
                if state.generation==generation && !cancel.is_cancelled() {state.status=status;changed.send_replace(generation);}
            };
            let publish=|resources:&Resources| {
                let mut state=shared.lock().expect("runtime state");
                if state.generation==generation && !cancel.is_cancelled() {
                    state.endpoints=resources.endpoints.clone();
                    state.status.services=resources.names.clone();
                    changed.send_replace(generation);
                }
            };
            let prepared=tokio::select! {_=cancel.cancelled()=>return,result=prepare(&plan,inference,&cancel,&update,&publish)=>result};
            let (mut resources, prepared_result) = prepared;
            // A failed optional download must not retire a recognizer that is
            // already serving an independent active feature.
            let mut issues = plan.issues.clone();
            if let Err(error) = prepared_result { issues.push(format!("{error:#}")); }
            {
                let mut state=shared.lock().expect("runtime state");
                if state.generation!=generation || cancel.is_cancelled() {return}
                state.endpoints=resources.endpoints.clone();state.status=RuntimeStatus {phase:if !issues.is_empty() {"error"} else if inference {"ready"} else {"cached"}.into(),message:(!issues.is_empty()).then(|| issues.join("; ")),services:resources.names.clone(),..Default::default()};changed.send_replace(generation);
            }
            if !inference { return; }
            loop {
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=> {if !resources.healthy() {
                    { let mut state=shared.lock().expect("runtime state"); if state.generation==generation { state.endpoints=Endpoints::default(); } }
                    update(RuntimeStatus {phase:"error".into(),message:Some("A bundled local inference service stopped; start again to restart it".into()),..Default::default()});break
                }}}
            }
            resources.stop().await;
        }));
        self.changed.send_replace(generation);
    }
    fn acquire(self: &Arc<Self>, desired: &Plan) -> Result<RuntimeLease> {
        let mut state = self.state.lock().expect("runtime state");
        if state.plan.as_ref() != Some(desired) || !state.inference {
            bail!("Local provider selection changed during preparation")
        }
        if let Some(task) = state.idle_task.take() {
            task.abort();
        }
        state.idle_epoch = state.idle_epoch.wrapping_add(1);
        state.leases += 1;
        Ok(RuntimeLease {
            manager: Arc::downgrade(self),
            generation: state.generation,
        })
    }

    fn release(&self, generation: u64) {
        let mut state = self.state.lock().expect("runtime state");
        if state.generation != generation || state.leases == 0 {
            return;
        }
        state.leases -= 1;
        if state.leases != 0 {
            return;
        }
        // An abandoned download/loading attempt must stop immediately. Only
        // ready models benefit from the reuse window between short sessions.
        let delay = if state.status.phase == "ready" {
            Duration::from_secs(state.plan.as_ref().map_or(0, |plan| plan.idle_unload_secs))
        } else {
            Duration::ZERO
        };
        state.idle_epoch = state.idle_epoch.wrapping_add(1);
        let idle_epoch = state.idle_epoch;
        let weak = Arc::downgrade(&self.state);
        let changed = self.changed.clone();
        let unload = move || {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let mut state = shared.lock().expect("runtime state");
            if state.generation != generation || state.leases != 0 || state.idle_epoch != idle_epoch
            {
                return;
            }
            state.cancel.cancel();
            state.generation += 1;
            state.inference = false;
            state.plan = None;
            state.endpoints = Endpoints::default();
            if state.status.phase != "error" {
                state.status = RuntimeStatus {
                    phase: if state.status.phase == "ready" {
                        "cached"
                    } else {
                        "idle"
                    }
                    .into(),
                    ..Default::default()
                };
            } else {
                state.status.services.clear();
            }
            changed.send_replace(state.generation);
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            state.idle_task = Some(handle.spawn(async move {
                tokio::time::sleep(delay).await;
                unload();
            }));
        } else {
            drop(state);
            unload();
        }
    }

    pub async fn resolve(
        self: &Arc<Self>,
        cfg: &AppConfig,
        cancel: CancellationToken,
    ) -> Result<(AppConfig, Option<RuntimeLease>)> {
        let active = Plan::active(cfg)?;
        if active.empty() {
            return Ok((cfg.clone(), None));
        }
        let mut updates = self.changed.subscribe();
        let reusable = {
            let state = self.state.lock().expect("runtime state");
            state.inference
                && state.plan.as_ref() == Some(&active)
                && active.satisfied_by(&state.endpoints)
        };
        if !reusable {
            self.reconcile_inner(Ok(active.clone()), true, true);
        }
        let lease = self.acquire(&active)?;
        loop {
            {
                let state = self.state.lock().expect("runtime state");
                if state.plan.as_ref() != Some(&active) || !state.inference {
                    bail!("Local provider selection changed during preparation")
                }
                if active.satisfied_by(&state.endpoints) {
                    return Ok((apply_endpoints(cfg, &state.endpoints)?, Some(lease)));
                }
                match state.status.phase.as_str() {
                    "ready" => {
                        bail!("Local inference preparation did not resolve an active provider")
                    }
                    "error" => bail!(
                        "{}",
                        state
                            .status
                            .message
                            .as_deref()
                            .unwrap_or("Local inference preparation failed")
                    ),
                    _ => {}
                }
            }
            tokio::select! {_=cancel.cancelled()=>bail!("Local provider preparation wait cancelled"),result=updates.changed()=>{result.context("Local inference manager stopped")?;}}
        }
    }
    pub async fn shutdown(&self) {
        let task = {
            let mut state = self.state.lock().expect("runtime state");
            state.cancel.cancel();
            if let Some(task) = state.idle_task.take() {
                task.abort();
            }
            state.leases = 0;
            state.inference = false;
            state.generation += 1;
            state.endpoints = Endpoints::default();
            state.plan = None;
            state.status = RuntimeStatus {
                phase: "idle".into(),
                ..Default::default()
            };
            self.changed.send_replace(state.generation);
            state.task.take()
        };
        if let Some(mut task) = task
            && tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}
fn apply_endpoints(cfg: &AppConfig, endpoints: &Endpoints) -> Result<AppConfig> {
    let mut resolved = cfg.clone();
    let local = &mut resolved.providers.local;
    if let Some(url) = endpoints.whisper.get(&local.whisper_model)
        && automatic(&local.whisper_endpoint)
    {
        local.whisper_endpoint = url.clone();
    }
    if let Some(url) = &endpoints.translation
        && automatic(&local.ollama_endpoint)
    {
        local.ollama_endpoint = url.clone();
        local.translation_api = "openai".into();
    }
    if let Some(url) = &endpoints.piper
        && automatic(&local.piper_endpoint)
    {
        local.piper_endpoint = url.clone();
        let catalog = Catalog::read();
        for (route, available) in [
            (&mut resolved.microphone, !cfg.microphone_uses_speaker()),
            (&mut resolved.speaker, true),
        ] {
            if available && route.enabled && route.provider == "local" {
                route.resolved_voice = catalog.voice("auto", &route.target_language)?;
            }
        }
    }
    let whisper = &mut resolved.transcription.providers.whisper;
    if let Some(url) = endpoints.whisper.get(&whisper.model)
        && automatic(&whisper.endpoint)
    {
        whisper.endpoint = url.clone();
        whisper.api_key_env.clear();
    }
    Ok(resolved)
}
struct Resources {
    endpoints: Endpoints,
    names: Vec<String>,
    http: Vec<ServiceProcess>,
    piper: Option<piper::PiperGateway>,
}
impl Resources {
    fn healthy(&mut self) -> bool {
        self.http.iter_mut().all(ServiceProcess::healthy)
            && self.piper.as_ref().is_none_or(|p| p.healthy())
    }
    async fn stop(self) {
        for process in self.http {
            process.stop().await;
        }
        drop(self.piper);
    }
}
async fn prepare(
    plan: &Plan,
    inference: bool,
    cancel: &CancellationToken,
    update: &impl Fn(RuntimeStatus),
    publish: &impl Fn(&Resources),
) -> (Resources, Result<()>) {
    let mut resources = Resources {
        endpoints: Endpoints::default(),
        names: Vec::new(),
        http: Vec::new(),
        piper: None,
    };
    let result = prepare_into(plan, inference, cancel, update, publish, &mut resources).await;
    (resources, result)
}

async fn prepare_into(
    plan: &Plan,
    inference: bool,
    cancel: &CancellationToken,
    update: &impl Fn(RuntimeStatus),
    publish: &impl Fn(&Resources),
    resources: &mut Resources,
) -> Result<()> {
    let catalog = Catalog::read();
    let cache = assets::cache_directory(&plan.directory)?;
    if !inference {
        let root = Bundle::locate()?;
        return cache_selected_assets(plan, &catalog, &cache, &root, cancel, update).await;
    }
    let bundle = Bundle::discover().await?;
    let mut errors = Vec::new();
    for id in &plan.whisper {
        let result: Result<ServiceProcess> = async {
            let model = obtain(&catalog.whisper[id], &cache, &bundle.root, cancel, update).await?;
            update(RuntimeStatus {
                phase: "preparing".into(),
                services: resources.names.clone(),
                ..Default::default()
            });
            let mut command = crate::execution::background_command(bundle.executable("whisper")?);
            command
                .args([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "0",
                    "-t",
                    &plan.threads.to_string(),
                    "-m",
                ])
                .arg(model);
            ServiceProcess::start(command, "babel-whisper", "/inference").await
        }
        .await;
        let process = match result {
            Ok(process) => process,
            Err(error) => {
                errors.push(format!("Whisper ({id}): {error:#}"));
                continue;
            }
        };
        resources
            .endpoints
            .whisper
            .insert(id.clone(), process.endpoint.clone());
        resources.http.push(process);
        resources.names.push(format!("Whisper ({id})"));
        publish(resources);
    }
    if let Some(id) = &plan.translation {
        let result: Result<ServiceProcess> = async {
            let model = obtain(
                &catalog.translation[id],
                &cache,
                &bundle.root,
                cancel,
                update,
            )
            .await?;
            update(RuntimeStatus {
                phase: "preparing".into(),
                services: resources.names.clone(),
                ..Default::default()
            });
            let mut command = crate::execution::background_command(bundle.executable("llama")?);
            command
                .args([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "0",
                    "--threads",
                    &plan.threads.to_string(),
                    "--ctx-size",
                    "4096",
                    "--parallel",
                    "1",
                    "--n-gpu-layers",
                    "0",
                    "--alias",
                    id,
                    "--model",
                ])
                .arg(model);
            ServiceProcess::start(command, "babel-llama", "/v1/chat/completions").await
        }
        .await;
        match result {
            Ok(process) => {
                resources.endpoints.translation = Some(process.endpoint.clone());
                resources.http.push(process);
                resources.names.push("Qwen3 0.6B".into());
                publish(resources);
            }
            Err(error) => errors.push(format!("Local translator: {error:#}")),
        }
    }
    if !plan.voices.is_empty() {
        let mut voices = Vec::new();
        for id in &plan.voices {
            let result: Result<_> = async {
                let voice = &catalog.voices[id];
                let model = obtain(&voice.model, &cache, &bundle.root, cancel, update).await?;
                let config = obtain(&voice.config, &cache, &bundle.root, cancel, update).await?;
                obtain(&voice.license, &cache, &bundle.root, cancel, update).await?;
                Ok((id.clone(), model, config))
            }
            .await;
            match result {
                Ok(voice) => voices.push(voice),
                Err(error) => errors.push(format!("Piper ({id}): {error:#}")),
            }
        }
        if !voices.is_empty() {
            let voice_ids = voices.iter().map(|(id, _, _)| id.clone()).collect();
            let result: Result<piper::PiperGateway> = async {
                update(RuntimeStatus {
                    phase: "preparing".into(),
                    services: resources.names.clone(),
                    ..Default::default()
                });
                let service = bundle
                    .services
                    .get("piper")
                    .context("Bundled Piper component is missing")?;
                let data = bundle.root.join(
                    service
                        .data
                        .as_deref()
                        .context("Bundled Piper language data is missing")?,
                );
                piper::start(&bundle.executable("piper")?, &data, voices, plan.threads).await
            }
            .await;
            match result {
                Ok(gateway) => {
                    resources.endpoints.piper = Some(gateway.endpoint.clone());
                    resources.endpoints.voices = voice_ids;
                    resources.piper = Some(gateway);
                    resources.names.push("Piper".into());
                    publish(resources);
                }
                Err(error) => errors.push(format!("Piper: {error:#}")),
            }
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}
/// Asset-only preparation deliberately has no executable/process dependency.
/// Selecting a provider can make its first use offline without holding its
/// inference state or model tensors in memory for the lifetime of the app.
async fn cache_selected_assets(
    plan: &Plan,
    catalog: &Catalog,
    cache: &Path,
    bundle: &Path,
    cancel: &CancellationToken,
    update: &impl Fn(RuntimeStatus),
) -> Result<()> {
    let mut selected = Vec::new();
    selected.extend(plan.whisper.iter().map(|id| &catalog.whisper[id]));
    if let Some(id) = &plan.translation {
        selected.push(&catalog.translation[id]);
    }
    for id in &plan.voices {
        let voice = &catalog.voices[id];
        selected.extend([&voice.model, &voice.config, &voice.license]);
    }
    let mut errors = Vec::new();
    for asset in selected {
        if let Err(error) = obtain(asset, cache, bundle, cancel, update).await {
            errors.push(format!("{}: {error:#}", asset.name));
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}

async fn obtain(
    asset: &Asset,
    cache: &Path,
    bundle: &Path,
    cancel: &CancellationToken,
    update: &impl Fn(RuntimeStatus),
) -> Result<std::path::PathBuf> {
    assets::acquire(asset, cache, bundle, cancel, |received, total| {
        update(RuntimeStatus {
            phase: "preparing".into(),
            download: Some(DownloadProgress {
                name: asset.name.clone(),
                received,
                total,
            }),
            ..Default::default()
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_mirroring_never_prepares_or_loads_remembered_microphone_models() {
        let mut cfg = AppConfig::default();
        cfg.audio.microphone_source = crate::config::MicrophoneSource::SpeakerOriginal;
        cfg.microphone.provider = "local".into();
        cfg.microphone.target_language = "xx".into();
        cfg.transcription.enabled = true;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        let selected = Plan::from_config(&cfg).unwrap();
        assert!(selected.empty());
        assert!(selected.issues.is_empty());
        assert!(Plan::active(&cfg).unwrap().empty());
        let endpoints = Endpoints {
            piper: Some("http://127.0.0.1:32145/synthesize".into()),
            ..Default::default()
        };
        let resolved = apply_endpoints(&cfg, &endpoints).unwrap();
        assert!(resolved.microphone.resolved_voice.is_empty());
        assert_eq!(resolved.microphone.target_language, "xx");
        cfg.speaker.provider = "local".into();
        assert!(!Plan::active(&cfg).unwrap().empty());
    }

    #[tokio::test]
    async fn active_stt_ignores_inactive_models_and_reuses_loaded_recognizer() {
        let manager = RuntimeManager::new();
        let mut cfg = AppConfig::default();
        cfg.microphone.provider = "local".into();
        cfg.microphone.enabled = false;
        cfg.microphone.target_language = "xx".into();
        cfg.speaker.enabled = false;
        cfg.transcription.enabled = true;
        cfg.transcription.speaker = false;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        let desired = Plan::from_config(&cfg).unwrap();
        assert!(
            !desired.issues.is_empty(),
            "unsupported inactive voice is diagnosed"
        );
        assert!(Plan::active(&cfg).unwrap().issues.is_empty());
        let preparation = {
            let mut state = manager.state.lock().unwrap();
            state.plan = Some(Plan::active(&cfg).unwrap());
            state.inference = true;
            state.generation = 7;
            state.status.phase = "preparing".into();
            state.cancel.clone()
        };
        let waiting = manager.clone();
        let selected = cfg.clone();
        let request =
            tokio::spawn(async move { waiting.resolve(&selected, CancellationToken::new()).await });
        tokio::task::yield_now().await;
        assert!(!request.is_finished());
        {
            let mut state = manager.state.lock().unwrap();
            state.endpoints.whisper.insert(
                cfg.transcription.providers.whisper.model.clone(),
                "http://127.0.0.1:49321/inference".into(),
            );
            manager.changed.send_replace(7);
        }
        let (resolved, _lease) = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.transcription.providers.whisper.endpoint,
            "http://127.0.0.1:49321/inference"
        );
        assert_eq!(cfg.transcription.providers.whisper.endpoint, "auto");
        assert_eq!(manager.status().phase, "preparing");
        assert!(!preparation.is_cancelled());
        {
            let mut state = manager.state.lock().unwrap();
            state.status.phase = "error".into();
            state.status.message = Some("Optional translator download failed".into());
        }
        manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            !preparation.is_cancelled(),
            "ready STT must survive an unrelated preparation failure"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn preparation_wait_is_cancellable_and_rejects_changed_selection() {
        let manager = RuntimeManager::new();
        let mut cfg = AppConfig::default();
        cfg.transcription.enabled = true;
        cfg.transcription.speaker = false;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        {
            let mut state = manager.state.lock().unwrap();
            state.plan = Some(Plan::active(&cfg).unwrap());
            state.inference = true;
            state.status.phase = "preparing".into();
        }
        let _other_demand = manager.acquire(&Plan::active(&cfg).unwrap()).unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), manager.resolve(&cfg, cancelled))
            .await
            .unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        let waiting = manager.clone();
        let request =
            tokio::spawn(async move { waiting.resolve(&cfg, CancellationToken::new()).await });
        tokio::task::yield_now().await;
        {
            let mut state = manager.state.lock().unwrap();
            state.plan = None;
            state.generation += 1;
            manager.changed.send_replace(state.generation);
        }
        let error = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("selection changed"));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn inactive_local_providers_never_block_original_recording_or_cloud_translation() {
        let manager = RuntimeManager::new();
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.speaker.provider = "local".into();
        cfg.speaker.target_language = "xx".into();
        cfg.recording.enabled = true;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        {
            let mut state = manager.state.lock().unwrap();
            state.plan = Some(Plan::from_config(&cfg).unwrap());
            state.status.phase = "error".into();
            state.status.message = Some("Inactive provider failed to prepare".into());
        }
        assert_eq!(manager.status().phase, "error");
        let original = serde_json::to_value(&cfg).unwrap();
        let (resolved, lease) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        assert!(lease.is_none());
        assert_eq!(serde_json::to_value(resolved).unwrap(), original);
        cfg.microphone.enabled = true;
        assert!(Plan::active(&cfg).unwrap().empty());
        manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        cfg.transcription.enabled = true;
        cfg.transcription.microphone = false;
        // Whisper remains selected for the unrecorded microphone, while the
        // speaker recognizer is Gemini; no local service is required.
        assert!(Plan::active(&cfg).unwrap().empty());
        manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        manager.shutdown().await;
    }
    #[test]
    fn selection_prepares_only_needed_components_without_audio() {
        let mut cfg = AppConfig::default();
        assert!(Plan::from_config(&cfg).unwrap().empty());
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        let plan = Plan::from_config(&cfg).unwrap();
        assert_eq!(plan.whisper.len(), 1);
        assert!(plan.translation.is_none());
        assert!(plan.voices.is_empty());
        cfg.microphone.provider = "local".into();
        let plan = Plan::from_config(&cfg).unwrap();
        assert_eq!(plan.whisper.len(), 1);
        assert!(plan.translation.is_some());
        assert_eq!(plan.voices.len(), 1);
    }
    #[test]
    fn resolves_per_route_voices_without_persisting_ports() {
        let mut cfg = AppConfig::default();
        cfg.microphone.provider = "local".into();
        cfg.speaker.provider = "local".into();
        cfg.microphone.target_language = "en-US".into();
        cfg.speaker.target_language = "pt-BR".into();
        let endpoints = Endpoints {
            piper: Some("http://127.0.0.1:32145/synthesize".into()),
            translation: Some("http://127.0.0.1:32146/v1/chat/completions".into()),
            ..Default::default()
        };
        let resolved = apply_endpoints(&cfg, &endpoints).unwrap();
        assert_eq!(resolved.microphone.resolved_voice, "en_US-lessac-medium");
        assert_eq!(resolved.speaker.resolved_voice, "pt_BR-faber-medium");
        assert_eq!(resolved.providers.local.translation_api, "openai");
        assert_eq!(cfg.providers.local.piper_endpoint, "auto");
        assert!(cfg.microphone.resolved_voice.is_empty());
    }
    #[test]
    fn synchronous_controller_construction_needs_no_async_runtime() {
        let manager = RuntimeManager::new();
        let mut cfg = AppConfig::default();
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        manager.reconcile(&cfg);
        assert_eq!(manager.status().phase, "idle");
    }
    fn ready_fixture(manager: &Arc<RuntimeManager>) -> (AppConfig, CancellationToken) {
        let mut cfg = AppConfig::default();
        cfg.microphone.enabled = false;
        cfg.speaker.enabled = false;
        cfg.transcription.enabled = true;
        cfg.transcription.speaker = false;
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        cfg.local_runtime.idle_unload_secs = 60;
        let mut state = manager.state.lock().unwrap();
        state.plan = Some(Plan::active(&cfg).unwrap());
        state.inference = true;
        state.status.phase = "ready".into();
        state.status.services = vec!["Whisper".into()];
        state.endpoints.whisper.insert(
            cfg.transcription.providers.whisper.model.clone(),
            "http://127.0.0.1:49321/inference".into(),
        );
        (cfg, state.cancel.clone())
    }

    #[tokio::test(start_paused = true)]
    async fn idle_models_release_memory_and_fast_restart_reuses_one_generation() {
        let manager = RuntimeManager::new();
        let (cfg, resources) = ready_fixture(&manager);
        let (_, first) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        let (_, second) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        drop(first);
        tokio::time::advance(Duration::from_secs(65)).await;
        assert!(
            !resources.is_cancelled(),
            "another demand still owns the model"
        );
        drop(second);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(30)).await;
        let (_, resumed) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        assert!(!resources.is_cancelled());
        tokio::time::advance(Duration::from_secs(65)).await;
        assert!(
            !resources.is_cancelled(),
            "old idle timers cannot stop a resumed session"
        );
        drop(resumed);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(61)).await;
        tokio::task::yield_now().await;
        assert!(resources.is_cancelled());
        assert_eq!(manager.status().phase, "cached");
        assert!(manager.status().services.is_empty());
        assert!(manager.state.lock().unwrap().endpoints.whisper.is_empty());
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_preparation_stops_immediately_and_old_lease_cannot_cancel_new_selection() {
        let manager = RuntimeManager::new();
        let (cfg, resources) = ready_fixture(&manager);
        {
            let mut state = manager.state.lock().unwrap();
            state.endpoints = Endpoints::default();
            state.status.phase = "preparing".into();
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(manager.resolve(&cfg, cancelled).await.is_err());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            resources.is_cancelled(),
            "cancelled model loading cannot run for the idle grace period"
        );
        assert_eq!(manager.status().phase, "idle");

        let (_, _) = ready_fixture(&manager);
        let (_, old_lease) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        manager.reconcile(&AppConfig::default());
        let (_, _) = ready_fixture(&manager);
        let fresh = CancellationToken::new();
        manager.state.lock().unwrap().cancel = fresh.clone();
        drop(old_lease);
        tokio::time::advance(Duration::from_secs(65)).await;
        assert!(
            !fresh.is_cancelled(),
            "stale leases cannot unload a different configuration"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn selection_caches_bundled_weights_without_an_inference_executable() {
        use sha2::{Digest, Sha256};
        let temporary = tempfile::tempdir().unwrap();
        let cache = temporary.path().join("cache");
        let bundle = temporary.path().join("bundle");
        std::fs::create_dir_all(bundle.join("models")).unwrap();
        let bytes = b"small offline fixture, never a real model";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let filename = format!("{}-fixture.bin", &hash[..16]);
        std::fs::write(bundle.join("models").join(&filename), bytes).unwrap();
        let catalog: Catalog = serde_json::from_value(serde_json::json!({
            "whisper": { "fixture": { "name": "fixture.bin", "size": bytes.len(), "sha256": hash, "url": "https://invalid.example/no-network" } },
            "translation": {}, "voices": {}
        })).unwrap();
        let mut plan = Plan::from_config(&AppConfig::default()).unwrap();
        plan.whisper.insert("fixture".into());
        cache_selected_assets(
            &plan,
            &catalog,
            &cache,
            &bundle,
            &CancellationToken::new(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(cache.join(&filename)).unwrap(), bytes);
        assert!(!bundle.join("bin").exists());
    }

    #[tokio::test]
    async fn external_profiles_never_start_managed_services() {
        let manager = RuntimeManager::new();
        let mut cfg = AppConfig::default();
        cfg.transcription.microphone_recognition.provider = "whisper".into();
        cfg.transcription.providers.whisper.endpoint = "http://127.0.0.1:32145/inference".into();
        let (resolved, lease) = manager
            .resolve(&cfg, CancellationToken::new())
            .await
            .unwrap();
        assert!(lease.is_none());
        assert_eq!(
            resolved.transcription.providers.whisper.endpoint,
            cfg.transcription.providers.whisper.endpoint
        );
        assert_eq!(manager.status().phase, "idle");
        manager.shutdown().await;
    }
}
