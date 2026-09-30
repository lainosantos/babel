//! OS tray: Linux StatusNotifier (no GTK), native event loops on Windows/macOS.
use crate::{
    audio::{self, Device, DeviceDirection},
    engine::Controller,
    i18n::{resolve_language, text},
    platform::PlatformInfo,
};
use anyhow::{Context, Result};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

static DASHBOARD_URL: OnceLock<Mutex<String>> = OnceLock::new();
pub fn set_dashboard_url(url: String) {
    *DASHBOARD_URL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = url;
}
pub(crate) fn clear_dashboard_url(url: &str) {
    let mut current = DASHBOARD_URL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if current.as_str() == url {
        current.clear();
    }
}
fn open_settings() {
    let url = DASHBOARD_URL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if !url.is_empty()
        && let Err(error) = open::that_detached(url)
    {
        tracing::warn!("Não foi possível abrir o navegador: {error}");
    }
}

const INPUT_PREFIX: &str = "physical-input:";
const OUTPUT_PREFIX: &str = "physical-output:";

#[derive(Debug, PartialEq, Eq)]
struct DeviceChoice {
    action: String,
    name: String,
    checked: bool,
}
fn physical_choices(
    devices: &[Device],
    direction: DeviceDirection,
    selected: &str,
) -> Vec<DeviceChoice> {
    let prefix = match direction {
        DeviceDirection::Input => INPUT_PREFIX,
        DeviceDirection::Output => OUTPUT_PREFIX,
    };
    devices
        .iter()
        .filter(|device| device.direction == direction && !device.is_virtual)
        .map(|device| DeviceChoice {
            action: format!("{prefix}{}", device.id),
            name: device.name.clone(),
            checked: device.id == selected,
        })
        .collect()
}
fn physical_action(id: &str) -> Option<(DeviceDirection, &str)> {
    [
        (INPUT_PREFIX, DeviceDirection::Input),
        (OUTPUT_PREFIX, DeviceDirection::Output),
    ]
    .into_iter()
    .find_map(|(prefix, direction)| {
        id.strip_prefix(prefix)
            .filter(|device| !device.is_empty())
            .map(|device| (direction, device))
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrayStatus {
    running: bool,
    routing_active: bool,
    error: bool,
    revision: u64,
}
fn tray_status_label(status: TrayStatus, language: &str) -> String {
    text(
        language,
        if status.error {
            "status.error"
        } else if status.running {
            "status.running"
        } else if status.routing_active {
            "status.routing"
        } else {
            "status.idle"
        },
    )
}
fn platform_label(language: &str) -> String {
    text(language, "platform").replace("{os}", PlatformInfo::current().name)
}
fn tray_tooltip(language: &str) -> String {
    text(language, "tooltip").replace("{os}", PlatformInfo::current().name)
}
#[derive(Debug)]
struct DeviceSnapshot {
    revision: u64,
    input: String,
    output: String,
    devices: std::result::Result<Vec<Device>, String>,
}
#[derive(Clone, Debug)]
struct TraySnapshot {
    language: String,
    status: TrayStatus,
    devices: Option<Arc<DeviceSnapshot>>,
    loading: bool,
}

impl Default for TraySnapshot {
    fn default() -> Self {
        Self {
            language: resolve_language("system"),
            status: TrayStatus::default(),
            devices: None,
            loading: false,
        }
    }
}

/// One bounded pending request and one latest-value snapshot: menu updates never
/// enqueue an unbounded inventory or enumerate hardware on each status tick.
fn monitor(
    controller: Arc<Controller>,
    runtime: &tokio::runtime::Handle,
    cancel: CancellationToken,
    language: String,
) -> (mpsc::Sender<()>, watch::Receiver<TraySnapshot>) {
    let (refresh, mut requests) = mpsc::channel(1);
    let initial = TraySnapshot {
        language,
        ..TraySnapshot::default()
    };
    let (updates, receiver) = watch::channel(initial.clone());
    runtime.spawn(async move {
        let mut state = initial;
        let mut known_revision = None;
        let mut pending = false;
        let mut job: Option<tokio::task::JoinHandle<DeviceSnapshot>> = None;
        let mut requested_revision = 0;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let needs_inventory = known_revision.is_some_and(|revision| {
                state.devices.as_ref().is_none_or(|devices| devices.revision < revision)
            });
            if job.is_none() && (pending || needs_inventory) {
                pending = false;
                requested_revision = known_revision.unwrap_or_default();
                let controller = controller.clone();
                job = Some(tokio::spawn(fetch_devices(controller, requested_revision)));
                state.loading = true;
                updates.send_replace(state.clone());
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    if let Some(job) = &job { job.abort(); }
                    break;
                }
                request = requests.recv() => {
                    if request.is_none() { break; }
                    pending = true;
                }
                result = async { job.as_mut().expect("guarded inventory task").await }, if job.is_some() => {
                    job = None;
                    state.devices = Some(Arc::new(result.unwrap_or_else(|_| DeviceSnapshot {
                        revision: requested_revision, input: String::new(), output: String::new(),
                        devices: Err("A consulta de dispositivos foi interrompida".into()),
                    })));
                    state.loading = false;
                    updates.send_replace(state.clone());
                }
                _ = tick.tick() => {
                    let status = controller.status().await;
                    let next = TrayStatus { running: status.running, routing_active: status.routing_active, error: status.last_error.is_some() || status.routing_error.is_some(), revision: status.config_revision };
                    let language = if known_revision != Some(next.revision) {
                        let (interface, revision) = controller.interface_snapshot().await;
                        // A concurrent save may fall between these snapshots.
                        // Publish only a language and status from one revision.
                        if revision != next.revision { continue; }
                        resolve_language(&interface.language)
                    } else {
                        state.language.clone()
                    };
                    known_revision = Some(next.revision);
                    if next != state.status || language != state.language {
                        state.language = language;
                        state.status = next;
                        updates.send_replace(state.clone());
                    }
                }
            }
        }
        if let Some(job) = job { job.abort(); }
    });
    (refresh, receiver)
}

async fn fetch_devices(controller: Arc<Controller>, requested_revision: u64) -> DeviceSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        // Read config/revision atomically. A concurrent edit during enumeration
        // causes another refresh, rather than labeling stale checks as current.
        let (config, revision) = controller.config_snapshot().await;
        DeviceSnapshot {
            revision,
            input: config.microphone.capture_device,
            output: config.speaker.playback_device,
            devices: audio::devices().await.map_err(|error| format!("{error:#}")),
        }
    })
    .await
    .unwrap_or_else(|_| DeviceSnapshot {
        revision: requested_revision,
        input: String::new(),
        output: String::new(),
        devices: Err("Tempo limite ao listar dispositivos de áudio".into()),
    })
}

fn physical_selection_enabled(state: &TraySnapshot, current_inventory: bool) -> bool {
    current_inventory && !state.loading
}

struct TrayUi {
    icon: TrayIcon,
    settings: MenuItem,
    exit: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    status: MenuItem,
    platform: MenuItem,
    physical_hint: MenuItem,
    physical_input: Submenu,
    physical_output: Submenu,
    refresh: MenuItem,
    device_items: Vec<(CheckMenuItem, bool)>,
    inventory: Option<Arc<DeviceSnapshot>>,
    state: TraySnapshot,
}
fn icon_rgba() -> Vec<u8> {
    // Pre-render the shared brand asset so every platform needs only a tiny
    // byte copy at startup, without image decoding or filesystem access.
    const ICON: &[u8; 32 * 32 * 4] = include_bytes!("../assets/babel-tray.rgba");
    ICON.to_vec()
}
impl TrayUi {
    fn new(language: String) -> Result<Self> {
        let menu = Menu::new();
        let status = MenuItem::with_id("status", text(&language, "status.connecting"), false, None);
        let platform = MenuItem::new(platform_label(&language), false, None);
        let settings = MenuItem::with_id("settings", text(&language, "settings"), true, None);
        let start = MenuItem::with_id("start", text(&language, "start"), true, None);
        let stop = MenuItem::with_id("stop", text(&language, "stop"), false, None);
        let physical_hint = MenuItem::new(text(&language, "physical.querying"), false, None);
        let physical_input = Submenu::new(text(&language, "physical.input"), true);
        let physical_output = Submenu::new(text(&language, "physical.output"), true);
        for submenu in [&physical_input, &physical_output] {
            submenu.append(&MenuItem::new(
                text(&language, "physical.loading"),
                false,
                None,
            ))?;
        }
        let refresh = MenuItem::with_id(
            "refresh-devices",
            text(&language, "physical.refresh"),
            true,
            None,
        );
        let exit = MenuItem::with_id("exit", text(&language, "exit"), true, None);
        menu.append_items(&[
            &status,
            &platform,
            &PredefinedMenuItem::separator(),
            &settings,
            &start,
            &stop,
            &PredefinedMenuItem::separator(),
            &physical_hint,
            &physical_input,
            &physical_output,
            &refresh,
            &PredefinedMenuItem::separator(),
            &exit,
        ])?;
        let icon = Icon::from_rgba(icon_rgba(), 32, 32)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip(tray_tooltip(&language))
            .build()?;
        Ok(Self {
            icon,
            settings,
            exit,
            start,
            stop,
            status,
            platform,
            physical_hint,
            physical_input,
            physical_output,
            refresh,
            device_items: Vec::new(),
            inventory: None,
            state: TraySnapshot {
                language,
                ..TraySnapshot::default()
            },
        })
    }
    fn update(&mut self, state: TraySnapshot) -> Result<()> {
        let language_changed = self.state.language != state.language;
        if language_changed {
            self.update_language(&state.language)?;
        }
        self.start.set_enabled(!state.status.running);
        self.stop.set_enabled(state.status.running);
        self.status
            .set_text(tray_status_label(state.status, &state.language));
        // Translate even stale inventory messages immediately; controls stay
        // disabled until the replacement inventory matches the config revision.
        let inventory = state
            .devices
            .as_ref()
            .filter(|inventory| inventory.revision == state.status.revision)
            .cloned()
            .or_else(|| language_changed.then(|| self.inventory.clone()).flatten());
        if let Some(inventory) = inventory
            && (language_changed
                || !self
                    .inventory
                    .as_ref()
                    .is_some_and(|old| Arc::ptr_eq(old, &inventory)))
        {
            self.replace_devices(&inventory, &state.language)?;
            self.inventory = Some(inventory);
        }
        self.state = state;
        self.restore_device_controls();
        Ok(())
    }
    fn update_language(&self, language: &str) -> Result<()> {
        self.platform.set_text(platform_label(language));
        self.settings.set_text(text(language, "settings"));
        self.start.set_text(text(language, "start"));
        self.stop.set_text(text(language, "stop"));
        self.exit.set_text(text(language, "exit"));
        self.refresh.set_text(text(language, "physical.refresh"));
        self.physical_input
            .set_text(text(language, "physical.input"));
        self.physical_output
            .set_text(text(language, "physical.output"));
        self.icon.set_tooltip(Some(tray_tooltip(language)))?;
        if self.inventory.is_none() {
            for submenu in [&self.physical_input, &self.physical_output] {
                while submenu.remove_at(0).is_some() {}
                submenu.append(&MenuItem::new(
                    text(language, "physical.loading"),
                    false,
                    None,
                ))?;
            }
        }
        Ok(())
    }
    fn restore_device_controls(&self) {
        let current = self
            .inventory
            .as_ref()
            .is_some_and(|devices| devices.revision == self.state.status.revision);
        let enabled = physical_selection_enabled(&self.state, current);
        for (item, checked) in &self.device_items {
            item.set_checked(*checked);
            item.set_enabled(enabled);
        }
        self.refresh.set_enabled(!self.state.loading);
        self.physical_hint.set_text(text(
            &self.state.language,
            if self.state.status.running {
                "physical.switch"
            } else if self.state.loading || !current {
                "physical.updating"
            } else {
                "physical.connected"
            },
        ));
    }
    fn replace_devices(&mut self, inventory: &DeviceSnapshot, language: &str) -> Result<()> {
        self.device_items.clear();
        for (submenu, direction, selected, empty) in [
            (
                &self.physical_input,
                DeviceDirection::Input,
                &inventory.input,
                "physical.no_input",
            ),
            (
                &self.physical_output,
                DeviceDirection::Output,
                &inventory.output,
                "physical.no_output",
            ),
        ] {
            while submenu.remove_at(0).is_some() {}
            match &inventory.devices {
                Ok(devices) => {
                    let choices = physical_choices(devices, direction, selected);
                    if choices.is_empty() {
                        submenu.append(&MenuItem::new(text(language, empty), false, None))?;
                    }
                    if !selected.is_empty() && !choices.iter().any(|choice| choice.checked) {
                        submenu.append(&MenuItem::new(
                            text(language, "physical.unavailable"),
                            false,
                            None,
                        ))?;
                    }
                    for choice in choices {
                        let item = CheckMenuItem::with_id(
                            choice.action,
                            choice.name.replace('&', "&&"),
                            true,
                            choice.checked,
                            None,
                        );
                        submenu.append(&item)?;
                        self.device_items.push((item, choice.checked));
                    }
                }
                Err(error) => {
                    tracing::warn!("Dispositivos da bandeja: {error}");
                    submenu.append(&MenuItem::new(
                        text(language, "physical.failed"),
                        false,
                        None,
                    ))?;
                }
            }
        }
        Ok(())
    }
}
fn apply_snapshot(
    ui: &mut TrayUi,
    state: TraySnapshot,
    controller: Arc<Controller>,
    runtime: &tokio::runtime::Handle,
) {
    if let Err(error) = ui.update(state) {
        tracing::warn!("Não foi possível atualizar o menu de áudio: {error:#}");
        runtime.spawn(async move {
            controller
                .report_error(format!("Menu de dispositivos: {error:#}"))
                .await;
        });
    }
}
fn dispatch(
    id: &str,
    controller: Arc<Controller>,
    runtime: &tokio::runtime::Handle,
    cancel: CancellationToken,
    refresh: &mpsc::Sender<()>,
) {
    if let Some((direction, device)) = physical_action(id) {
        let device = device.to_owned();
        let refresh = refresh.clone();
        runtime.spawn(async move {
            if let Err(error) = controller.select_physical_device(direction, device).await {
                tracing::warn!("Seleção de dispositivo na bandeja: {error:#}");
                controller.report_error(format!("{error:#}")).await;
                open_settings();
            }
            // Also restores checks after a rejected click whose native checkbox
            // was toggled automatically, even when config_revision did not change.
            let _ = refresh.try_send(());
        });
        return;
    }
    match id {
        "settings" => open_settings(),
        "refresh-devices" => {
            let _ = refresh.try_send(());
        }
        "exit" => cancel.cancel(),
        "start" | "stop" => {
            let start = id == "start";
            runtime.spawn(async move {
                let result = if start {
                    controller.start().await
                } else {
                    controller.stop().await
                };
                if let Err(error) = result {
                    tracing::warn!("Controle da bandeja: {error:#}");
                    controller.report_error(format!("{error:#}")).await;
                    open_settings();
                }
            });
        }
        _ => {}
    }
}

#[cfg(target_os = "linux")]
fn wait_for_linux_tray<T>(
    cancel: &CancellationToken,
    mut create: impl FnMut() -> Result<T>,
    mut wait: impl FnMut(Duration) -> bool,
) -> Option<T> {
    let mut warned = false;
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        match create() {
            Ok(tray) => {
                if warned {
                    tracing::info!("Bandeja registrada após o desktop ficar disponível");
                }
                return Some(tray);
            }
            Err(error) => {
                if !warned {
                    tracing::warn!(
                        "Bandeja indisponível; nova tentativa em 3 s. O painel permanece disponível: {error:#}"
                    );
                    warned = true;
                }
            }
        }
        if !wait(Duration::from_secs(3)) {
            return None;
        }
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_linux_retry(cancel: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[cfg(target_os = "linux")]
pub async fn start(
    controller: Arc<Controller>,
    cancel: CancellationToken,
) -> Result<std::thread::JoinHandle<()>> {
    let runtime = tokio::runtime::Handle::current();
    let (interface, _) = controller.interface_snapshot().await;
    let language = resolve_language(&interface.language);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<std::result::Result<(), String>>();
    let handle = std::thread::Builder::new()
        .name("babel-tray".into())
        .spawn(move || {
            // Desktop registration can be unavailable until login/unlock. The
            // dedicated worker is ready even while it waits for that service.
            let _ = ready_tx.send(Ok(()));
            // Keep construction outside Tokio: ksni's blocking API owns its own
            // runtime. Only the cancellable retry delay enters our runtime.
            let Some(mut ui) = wait_for_linux_tray(
                &cancel,
                || TrayUi::new(language.clone()),
                |delay| runtime.block_on(wait_for_linux_retry(&cancel, delay)),
            ) else {
                return;
            };
            let tray_cancel = cancel.child_token();
            let _guard = tray_cancel.clone().drop_guard();
            let (refresh, mut updates) =
                monitor(controller.clone(), &runtime, tray_cancel, language);
            while !cancel.is_cancelled() {
                if let Ok(event) = MenuEvent::receiver().recv_timeout(Duration::from_millis(250)) {
                    // Native checkboxes toggle before dispatch. Keep the saved choice
                    // marked until Controller validates and persists the new one.
                    ui.restore_device_controls();
                    dispatch(
                        event.id.as_ref(),
                        controller.clone(),
                        &runtime,
                        cancel.clone(),
                        &refresh,
                    );
                }
                if updates.has_changed().unwrap_or(false) {
                    let state = updates.borrow_and_update().clone();
                    apply_snapshot(&mut ui, state, controller.clone(), &runtime);
                }
            }
        })?;
    ready_rx
        .await
        .context("Não foi possível iniciar a bandeja")?
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(handle)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn run_native(path: std::path::PathBuf, port: u16) -> Result<()> {
    use tao::{
        event::{Event, StartCause},
        event_loop::{ControlFlow, EventLoopBuilder},
        platform::run_return::EventLoopExtRunReturn,
    };
    let instance = crate::dashboard::InstanceGuard::acquire(&path)?;
    let config = if instance.config_path().exists() {
        crate::config::AppConfig::load(instance.config_path())?
    } else {
        crate::config::AppConfig::default()
    };
    #[derive(Debug)]
    enum UiEvent {
        Menu(MenuEvent),
        Refresh,
        Exit,
    }
    let mut builder = EventLoopBuilder::<UiEvent>::with_user_event();
    let mut event_loop = builder.build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let language = resolve_language(&config.interface.language);
    let controller = Arc::new(Controller::new(config, instance.config_path().to_owned())?);
    let cancel = CancellationToken::new();
    let server_controller = controller.clone();
    let server_cancel = cancel.clone();
    let proxy = event_loop.create_proxy();
    let server = runtime.spawn(async move {
        let result =
            crate::dashboard::serve(server_controller.clone(), port, server_cancel.clone()).await;
        server_cancel.cancel();
        let _ = server_controller.shutdown().await;
        let _ = proxy.send_event(UiEvent::Exit);
        result
    });
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UiEvent::Menu(event));
    }));
    let (refresh, mut updates) = monitor(
        controller.clone(),
        runtime.handle(),
        cancel.clone(),
        language.clone(),
    );
    let mut changed = updates.clone();
    let proxy = event_loop.create_proxy();
    let monitor_cancel = cancel.clone();
    runtime.spawn(async move {
        loop {
            tokio::select! {
                _ = monitor_cancel.cancelled() => { let _ = proxy.send_event(UiEvent::Exit); break; }
                result = changed.changed() => {
                    if result.is_err() { break; }
                    let _ = proxy.send_event(UiEvent::Refresh);
                }
            }
        }
    });
    let signal = cancel.clone();
    runtime.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    let mut tray = None;
    let mut tray_error = None;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => match TrayUi::new(language.clone()) {
                Ok(mut ui) => {
                    apply_snapshot(
                        &mut ui,
                        updates.borrow_and_update().clone(),
                        controller.clone(),
                        runtime.handle(),
                    );
                    tray = Some(ui);
                }
                Err(error) => {
                    tray_error = Some(error);
                    cancel.cancel();
                    *flow = ControlFlow::Exit;
                }
            },
            Event::UserEvent(UiEvent::Menu(event)) => {
                if let Some(ui) = &tray {
                    ui.restore_device_controls();
                }
                dispatch(
                    event.id.as_ref(),
                    controller.clone(),
                    runtime.handle(),
                    cancel.clone(),
                    &refresh,
                );
            }
            Event::UserEvent(UiEvent::Refresh) => {
                if let Some(ui) = &mut tray {
                    apply_snapshot(
                        ui,
                        updates.borrow_and_update().clone(),
                        controller.clone(),
                        runtime.handle(),
                    );
                }
            }
            Event::UserEvent(UiEvent::Exit) => {
                tray.take();
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
    cancel.cancel();
    runtime.block_on(controller.shutdown())?;
    runtime
        .block_on(server)
        .context("Servidor do painel interrompido")??;
    if let Some(error) = tray_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_url_is_published_from_the_server_and_cleared_only_by_its_owner() {
        let first = "http://127.0.0.1:31415/#token=first".to_owned();
        let second = "http://127.0.0.1:27182/#token=second".to_owned();
        set_dashboard_url(first.clone());
        set_dashboard_url(second.clone());
        clear_dashboard_url(&first);
        assert_eq!(*DASHBOARD_URL.get().unwrap().lock().unwrap(), second);
        clear_dashboard_url(&second);
        assert!(DASHBOARD_URL.get().unwrap().lock().unwrap().is_empty());
    }
    // Run explicitly inside an isolated bus without a StatusNotifierWatcher:
    // dbus-run-session -- cargo test --lib linux_tray_worker_waits_for_desktop -- --ignored --nocapture
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an isolated dbus-run-session without a desktop watcher"]
    async fn linux_tray_worker_waits_for_desktop_without_blocking_start_or_cancellation() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .try_init();
        let directory = tempfile::tempdir().unwrap();
        let controller = Arc::new(
            Controller::new(
                crate::config::AppConfig::default(),
                directory.path().join("config.toml"),
            )
            .unwrap(),
        );
        let cancel = CancellationToken::new();
        let worker = tokio::time::timeout(
            Duration::from_secs(1),
            super::start(controller.clone(), cancel.clone()),
        )
        .await
        .expect("worker readiness must not wait for desktop registration")
        .unwrap();
        // Exercise the real ksni constructor and another retry, without audio
        // routing, providers, filesystem configuration or desktop mutations.
        tokio::time::sleep(Duration::from_millis(3250)).await;
        assert!(
            !worker.is_finished(),
            "tray worker exited instead of retrying"
        );
        assert!(!controller.status().await.running);
        cancel.cancel();
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || worker.join()),
        )
        .await
        .expect("tray retry must stop promptly")
        .unwrap()
        .expect("tray worker must not panic, including from nested runtimes");
        assert!(!directory.path().join("config.toml").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_tray_retries_unavailable_desktop_then_registers() {
        let cancel = CancellationToken::new();
        let mut attempts = 0;
        let mut waits = Vec::new();
        let library_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let tray = wait_for_linux_tray(
            &cancel,
            || {
                // Mimic ksni's blocking API: construction must be allowed to
                // drive a separate runtime without a nested-runtime panic.
                library_runtime.block_on(async {});
                attempts += 1;
                if attempts < 3 {
                    anyhow::bail!("fixture: StatusNotifierWatcher unavailable");
                }
                Ok("registered fixture")
            },
            |delay| {
                waits.push(delay);
                true
            },
        );
        assert_eq!(tray, Some("registered fixture"));
        assert_eq!(attempts, 3);
        assert_eq!(waits, vec![Duration::from_secs(3); 2]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn linux_tray_retry_wait_is_cancelled_without_waiting_three_seconds() {
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            signal.cancel();
        });
        let began = tokio::time::Instant::now();
        assert!(!wait_for_linux_retry(&cancel, Duration::from_secs(3)).await);
        assert!(began.elapsed() <= Duration::from_millis(250));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_tray_does_not_attempt_registration_after_cancellation() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = wait_for_linux_tray::<()>(
            &cancel,
            || panic!("cancelled worker must not register a tray"),
            |_| panic!("cancelled worker must not wait"),
        );
        assert_eq!(result, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_tray_stops_retrying_when_wait_is_cancelled() {
        let cancel = CancellationToken::new();
        let mut attempts = 0;
        let result = wait_for_linux_tray::<()>(
            &cancel,
            || {
                attempts += 1;
                anyhow::bail!("fixture: desktop locked")
            },
            |_| {
                cancel.cancel();
                false
            },
        );
        assert_eq!(result, None);
        assert_eq!(attempts, 1);
    }

    #[test]
    fn tray_distinguishes_sessions_from_original_routing_and_shows_errors() {
        let mut status = TrayStatus::default();
        assert_eq!(tray_status_label(status, "pt"), "Babel · sem roteamento");
        status.routing_active = true;
        assert_eq!(tray_status_label(status, "pt"), "Babel · áudio original");
        status.running = true;
        assert_eq!(tray_status_label(status, "pt"), "Babel · sessão ativa");
        status.error = true;
        assert_eq!(
            tray_status_label(status, "pt"),
            "Babel · erro, abra as configurações"
        );
    }
    #[tokio::test]
    async fn tray_monitor_observes_saved_language_without_restarting() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::config::AppConfig::default();
        config.interface.language = "en".into();
        let controller =
            Arc::new(Controller::new(config, directory.path().join("config.toml")).unwrap());
        let cancel = CancellationToken::new();
        let _guard = cancel.clone().drop_guard();
        let (_refresh, mut updates) = monitor(
            controller.clone(),
            &tokio::runtime::Handle::current(),
            cancel.clone(),
            "en".into(),
        );
        assert_eq!(updates.borrow().language, "en");
        controller
            .set_interface_language("pt".into(), Some(0))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                updates.changed().await.unwrap();
                let state = updates.borrow_and_update().clone();
                if state.language == "pt" {
                    assert_eq!(state.status.revision, 1);
                    assert!(!state.status.running);
                    break;
                }
            }
        })
        .await
        .expect("tray language should follow saved interface settings");
        cancel.cancel();
        assert!(!controller.status().await.running);
    }

    #[test]
    fn language_switch_preserves_session_state_and_menu_action_identity() {
        let status = TrayStatus {
            running: true,
            routing_active: true,
            error: false,
            revision: 9,
        };
        assert_eq!(tray_status_label(status, "en"), "Babel · session active");
        assert_eq!(tray_status_label(status, "pt"), "Babel · sessão ativa");
        assert_eq!(
            tray_status_label(TrayStatus::default(), "en"),
            "Babel · no routing"
        );
        let routing = TrayStatus {
            running: false,
            ..status
        };
        assert_eq!(tray_status_label(routing, "en"), "Babel · original audio");
        assert_eq!(
            tray_status_label(
                TrayStatus {
                    error: true,
                    ..status
                },
                "en"
            ),
            "Babel · error, open settings"
        );

        let mut state = TraySnapshot {
            status,
            language: "pt".into(),
            ..TraySnapshot::default()
        };
        state.language = "en".into();
        assert!(physical_selection_enabled(&state, true));
        assert!(state.status.running);
        let devices = vec![device("USB:original-id", DeviceDirection::Input, false)];
        let choice =
            physical_choices(&devices, DeviceDirection::Input, "USB:original-id").remove(0);
        assert_eq!(choice.name, "Device USB:original-id");
        assert_eq!(
            physical_action(&choice.action),
            Some((DeviceDirection::Input, "USB:original-id"))
        );
    }
    fn device(id: &str, direction: DeviceDirection, is_virtual: bool) -> Device {
        Device {
            id: id.into(),
            name: format!("Device {id}"),
            direction,
            is_virtual,
        }
    }
    #[test]
    fn icon_has_valid_dimensions_and_visible_signal() {
        let rgba = icon_rgba();
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert!(rgba.as_chunks::<4>().0.iter().any(|p| p[3] == 255));
        assert!(rgba.as_chunks::<4>().0.iter().any(|p| p[3] == 0));
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] == 255 && p[..3].iter().all(|channel| *channel >= 245)),
            "the brand glyph must be visible, not just the background"
        );
    }
    #[test]
    fn physical_menus_filter_virtual_and_wrong_direction_and_mark_saved_choice() {
        let devices = vec![
            device("input:0:USB", DeviceDirection::Input, false),
            device("input:1:Headset", DeviceDirection::Input, false),
            device("babel_microphone", DeviceDirection::Input, true),
            device("babel_speaker", DeviceDirection::Output, true),
            device("output:2:Headset", DeviceDirection::Output, false),
        ];
        let inputs = physical_choices(&devices, DeviceDirection::Input, "input:1:Headset");
        assert_eq!(inputs.len(), 2);
        assert!(!inputs[0].checked);
        assert!(inputs[1].checked);
        assert_eq!(
            physical_action(&inputs[1].action),
            Some((DeviceDirection::Input, "input:1:Headset"))
        );
        let outputs = physical_choices(&devices, DeviceDirection::Output, "output:2:Headset");
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].checked);
        assert_eq!(
            physical_action(&outputs[0].action),
            Some((DeviceDirection::Output, "output:2:Headset"))
        );
    }
    #[test]
    fn device_actions_keep_full_identity_when_inventory_order_changes() {
        let mut devices = vec![
            device("alsa_input.usb:serial", DeviceDirection::Input, false),
            device("alsa_input.pci", DeviceDirection::Input, false),
        ];
        let first =
            physical_choices(&devices, DeviceDirection::Input, "alsa_input.usb:serial").remove(0);
        devices.reverse();
        let reordered =
            physical_choices(&devices, DeviceDirection::Input, "alsa_input.usb:serial").remove(1);
        assert_eq!(first, reordered);
        for invalid in ["physical-input:", "physical-output:", "input:0", "settings"] {
            assert!(physical_action(invalid).is_none());
        }
    }
    #[test]
    fn live_session_keeps_physical_selection_available() {
        let mut state = TraySnapshot::default();
        assert!(physical_selection_enabled(&state, true));
        state.status.running = true;
        assert!(physical_selection_enabled(&state, true));
        state.loading = true;
        assert!(!physical_selection_enabled(&state, true));
        state.loading = false;
        assert!(!physical_selection_enabled(&state, false));
    }
}
