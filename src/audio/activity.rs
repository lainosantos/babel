//! Observe virtual-endpoint consumers and the selected system microphone without
//! opening a hardware stream. Speaker activity always requires an external client.
//! Linux combines PulseAudio subscription events with bounded, periodic snapshots.

#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(target_os = "windows", test))]
mod windows_policy;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndpointUse {
    pub microphone: bool,
    pub speaker: bool,
    /// Output selection may remain valid after the last active stream ends.
    pub speaker_selected: bool,
    pub microphone_epoch: u64,
    pub speaker_epoch: u64,
    pub speaker_selection_epoch: u64,
    pub microphone_error: Option<String>,
    pub speaker_error: Option<String>,
    pub error: Option<String>,
}

/// The first watch change confirms an actual device observation, including an
/// unused microphone. Consumers can wait without treating startup as inactivity.
/// Epochs retain selection changes even if watch updates have been coalesced.
pub(crate) fn monitor_initializing(
    mic_playback: watch::Receiver<String>,
    speaker_capture: watch::Receiver<String>,
    cancel: CancellationToken,
) -> watch::Receiver<EndpointUse> {
    monitor_from(
        mic_playback,
        speaker_capture,
        cancel,
        EndpointUse {
            error: Some("Inspecting virtual audio endpoint use".into()),
            ..Default::default()
        },
    )
}

fn monitor_from(
    mic_playback: watch::Receiver<String>,
    speaker_capture: watch::Receiver<String>,
    cancel: CancellationToken,
    initial: EndpointUse,
) -> watch::Receiver<EndpointUse> {
    #[cfg(target_os = "linux")]
    {
        let (sender, receiver) = watch::channel(initial);
        tokio::spawn(linux::run(mic_playback, speaker_capture, cancel, sender));
        receiver
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let (sender, receiver) = watch::channel(initial);
        #[cfg(target_os = "macos")]
        tokio::spawn(macos::run(mic_playback, speaker_capture, cancel, sender));
        #[cfg(target_os = "windows")]
        tokio::spawn(windows::run(mic_playback, speaker_capture, cancel, sender));
        receiver
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{collections::HashSet, process::Stdio, time::Duration};

    use anyhow::{Context, Result, bail, ensure};
    use serde_json::Value;
    use tokio::{
        io::{AsyncRead, AsyncReadExt},
        process::Command,
        sync::{mpsc, watch},
    };
    use tokio_util::sync::CancellationToken;

    use super::EndpointUse;

    const OWNER: &str = "org.babel.audio.v1";
    const APP_ID: &str = "org.babel.audio";
    const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
    const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
    const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

    #[derive(Default, Debug)]
    struct UseSnapshot {
        microphone_default: bool,
        speaker_default: bool,
        microphone_clients: HashSet<u64>,
        speaker_clients: HashSet<u64>,
        speaker_selected_clients: HashSet<u64>,
        microphone_error: Option<String>,
        speaker_error: Option<String>,
    }

    impl UseSnapshot {
        fn microphone_active(&self) -> bool {
            self.microphone_default || !self.microphone_clients.is_empty()
        }
        fn speaker_selected(&self) -> bool {
            self.speaker_default || !self.speaker_selected_clients.is_empty()
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Event {
        Refresh,
        RemovedSourceOutput(u64),
        RemovedSinkInput(u64),
    }

    pub(super) async fn run(
        mut mic_playback: watch::Receiver<String>,
        mut speaker_capture: watch::Receiver<String>,
        cancel: CancellationToken,
        state: watch::Sender<EndpointUse>,
    ) {
        let (events_tx, mut events) = mpsc::channel(64);
        let subscription_cancel = cancel.child_token();
        let subscription = tokio::spawn(subscribe(events_tx, subscription_cancel.clone()));
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut snapshot = UseSnapshot::default();
        let mut mic_watch_open = true;
        let mut speaker_watch_open = true;
        let mut next_snapshot = tokio::time::Instant::now();

        loop {
            {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = state.closed() => break,
                    _ = tokio::time::sleep_until(next_snapshot) => (),
                }
                next_snapshot = tokio::time::Instant::now() + Duration::from_millis(40);
                for _ in 0..64 {
                    let Ok(event) = events.try_recv() else {
                        break;
                    };
                    apply_event(event, &mut snapshot, &state);
                }
                let mic_device = mic_playback.borrow_and_update().clone();
                let speaker_device = speaker_capture.borrow_and_update().clone();
                let inspected = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = state.closed() => break,
                    result = inspect(&mic_device, &speaker_device) => result,
                };
                // Preserve the inactive edge when a client closes and reopens
                // while pactl is producing its snapshot. The two watch updates
                // can coalesce, but their epochs cannot.
                let mut trailing_refresh = false;
                let mut pending_removals = Vec::with_capacity(64);
                for _ in 0..64 {
                    let Ok(event) = events.try_recv() else {
                        break;
                    };
                    apply_event(event, &mut snapshot, &state);
                    pending_removals.push(event);
                    trailing_refresh = true;
                }
                if mic_playback.has_changed().unwrap_or(false)
                    || speaker_capture.has_changed().unwrap_or(false)
                {
                    if mic_playback.has_changed().unwrap_or(false) {
                        invalidate_direction(&state, true);
                    }
                    if speaker_capture.has_changed().unwrap_or(false) {
                        invalidate_direction(&state, false);
                    }
                    continue;
                }
                match inspected {
                    Ok(mut current) => {
                        for event in pending_removals {
                            match event {
                                Event::RemovedSourceOutput(id) => {
                                    current.microphone_clients.remove(&id);
                                }
                                Event::RemovedSinkInput(id) => {
                                    current.speaker_clients.remove(&id);
                                    current.speaker_selected_clients.remove(&id);
                                }
                                Event::Refresh => (),
                            }
                        }
                        publish_state(
                            &state,
                            current.microphone_active(),
                            !current.speaker_clients.is_empty(),
                            current.speaker_selected(),
                            current.microphone_error.clone(),
                            current.speaker_error.clone(),
                            None,
                        );
                        snapshot = current;
                    }
                    Err(error) => {
                        snapshot = UseSnapshot::default();
                        publish_state(
                            &state,
                            false,
                            false,
                            false,
                            None,
                            None,
                            Some(format!(
                                "Could not inspect Babel virtual-device activity: {error:#}"
                            )),
                        );
                    }
                }
                if trailing_refresh {
                    continue;
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = state.closed() => break,
                result = mic_playback.changed(), if mic_watch_open => {
                    mic_watch_open = result.is_ok();
                    // Never keep a route open using activity from its old endpoint.
                    invalidate_direction(&state, true);
                    snapshot.microphone_clients.clear();
                    snapshot.microphone_default = false;
                }
                result = speaker_capture.changed(), if speaker_watch_open => {
                    speaker_watch_open = result.is_ok();
                    invalidate_direction(&state, false);
                    snapshot.speaker_clients.clear();
                    snapshot.speaker_selected_clients.clear();
                    snapshot.speaker_default = false;
                }
                event = events.recv() => {
                    match event {
                        Some(event) => apply_event(event, &mut snapshot, &state),
                        None => break,
                    }
                }
                _ = interval.tick() => (),
            }
        }
        publish_state(&state, false, false, false, None, None, None);
        subscription_cancel.cancel();
        let _ = subscription.await;
    }

    fn apply_event(event: Event, snapshot: &mut UseSnapshot, state: &watch::Sender<EndpointUse>) {
        match event {
            Event::RemovedSourceOutput(id) => {
                if snapshot.microphone_clients.remove(&id) && !snapshot.microphone_active() {
                    let current = state.borrow().clone();
                    publish_state(
                        state,
                        false,
                        current.speaker,
                        current.speaker_selected,
                        current.microphone_error,
                        current.speaker_error,
                        current.error,
                    );
                }
            }
            Event::RemovedSinkInput(id) => {
                let removed = snapshot.speaker_selected_clients.remove(&id);
                if snapshot.speaker_clients.remove(&id) || removed {
                    let current = state.borrow().clone();
                    publish_state(
                        state,
                        current.microphone,
                        !snapshot.speaker_clients.is_empty(),
                        snapshot.speaker_selected(),
                        current.microphone_error,
                        current.speaker_error,
                        current.error,
                    );
                }
            }
            Event::Refresh => (),
        }
    }

    #[cfg(test)]
    fn publish(
        state: &watch::Sender<EndpointUse>,
        microphone: bool,
        speaker: bool,
        microphone_error: Option<String>,
        speaker_error: Option<String>,
        error: Option<String>,
    ) {
        publish_state(
            state,
            microphone,
            speaker,
            speaker,
            microphone_error,
            speaker_error,
            error,
        );
    }

    fn publish_state(
        state: &watch::Sender<EndpointUse>,
        microphone: bool,
        speaker: bool,
        speaker_selected: bool,
        microphone_error: Option<String>,
        speaker_error: Option<String>,
        error: Option<String>,
    ) {
        state.send_if_modified(|current| {
            let error_changed = current.error != error;
            let mic_changed = current.microphone != microphone
                || current.microphone_error != microphone_error
                || error_changed;
            let speaker_changed = current.speaker != speaker
                || current.speaker_error != speaker_error
                || error_changed;
            let selection_changed = current.speaker_selected != speaker_selected
                || current.speaker_error != speaker_error
                || error_changed;
            if mic_changed {
                current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
            }
            if speaker_changed {
                current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
            }
            if selection_changed {
                current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
            }
            current.microphone = microphone;
            current.speaker = speaker;
            current.speaker_selected = speaker_selected;
            current.microphone_error = microphone_error;
            current.speaker_error = speaker_error;
            current.error = error;
            mic_changed || speaker_changed || selection_changed
        });
    }

    fn invalidate_direction(state: &watch::Sender<EndpointUse>, microphone: bool) {
        state.send_modify(|current| {
            if microphone {
                current.microphone = false;
                current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
            } else {
                current.speaker = false;
                current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
                current.speaker_selected = false;
                current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
            }
        });
    }

    async fn read_limited(input: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(8192.min(limit));
        input
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= limit,
            "audio-server inspection exceeded its bounded output limit"
        );
        Ok(bytes)
    }

    async fn pactl(args: &[&str], limit: usize) -> Result<Vec<u8>> {
        let mut child = Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("could not run pactl; install the PulseAudio client tools")?;
        let stdout = child.stdout.take().context("pactl stdout unavailable")?;
        let stderr = child.stderr.take().context("pactl stderr unavailable")?;
        let received = tokio::time::timeout(COMMAND_TIMEOUT, async {
            let (stdout, stderr, status) = tokio::try_join!(
                read_limited(stdout, limit),
                read_limited(stderr, 8192),
                async { child.wait().await.context("could not wait for pactl") },
            )?;
            ensure!(
                status.success(),
                "pactl inspection failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            );
            Ok(stdout)
        })
        .await;
        match received {
            Ok(result) => {
                if result.is_err() {
                    let _ = child.kill().await;
                }
                result
            }
            Err(_) => {
                let _ = child.kill().await;
                bail!("pactl inspection timed out; check the PulseAudio or pipewire-pulse server")
            }
        }
    }

    async fn inspect(mic_device: &str, speaker_device: &str) -> Result<UseSnapshot> {
        let bytes = pactl(&["--format=json", "list"], MAX_SNAPSHOT_BYTES).await?;
        let mut snapshot: Value =
            serde_json::from_slice(&bytes).context("invalid pactl JSON activity snapshot")?;
        // Some pactl 17 builds omit indices only from JSON module records. The
        // bounded short listing preserves the identifiers needed to exclude the
        // remap module's own source-output; these are not microphone consumers.
        let modules = array(&snapshot, "modules")?;
        if modules
            .iter()
            .any(|module| index(&module["index"]).is_none())
        {
            let short = pactl(&["list", "short", "modules"], MAX_SNAPSHOT_BYTES).await?;
            snapshot["modules"] = Value::Array(parse_short_modules(&short)?);
        }
        // Output default selection authorizes an already accepted translation
        // tail. It never authorizes opening speaker capture without a client.
        let (default_source, default_sink) = tokio::join!(
            pactl(&["get-default-source"], 4096),
            pactl(&["get-default-sink"], 4096)
        );
        let default_source = default_source.and_then(|bytes| {
            String::from_utf8(bytes).context("default microphone name was not UTF-8")
        });
        let default_sink = default_sink.and_then(|bytes| {
            String::from_utf8(bytes).context("default speaker name was not UTF-8")
        });
        let mut result = evaluate(
            &snapshot,
            mic_device,
            speaker_device,
            default_source.as_deref().ok().map(str::trim),
        )?;
        if let Err(error) = default_source {
            result.microphone_clients.clear();
            result.microphone_error = Some(format!(
                "Could not inspect the system microphone selection: {error:#}"
            ));
        }
        match default_sink {
            Ok(name) => select_default_speaker(&mut result, &snapshot, speaker_device, name.trim()),
            Err(error) => {
                result.speaker_error = Some(format!(
                    "Could not inspect the system speaker selection: {error:#}"
                ));
            }
        }
        Ok(result)
    }

    fn select_default_speaker(
        result: &mut UseSnapshot,
        snapshot: &Value,
        speaker_device: &str,
        default_sink: &str,
    ) {
        if result.speaker_error.is_some() {
            return;
        }
        let sinks = snapshot["sinks"]
            .as_array()
            .expect("validated activity snapshot");
        let sources = snapshot["sources"]
            .as_array()
            .expect("validated activity snapshot");
        let selected = speaker_sink(sinks, sources, speaker_device).ok();
        result.speaker_default = selected.is_some()
            && sinks.iter().any(|sink| {
                sink["name"].as_str() == Some(default_sink) && index(&sink["index"]) == selected
            });
    }

    fn parse_short_modules(bytes: &[u8]) -> Result<Vec<Value>> {
        let text = std::str::from_utf8(bytes).context("pactl module listing was not UTF-8")?;
        text.lines()
            .filter(|line| {
                line.split_once('\t')
                    .is_some_and(|(id, _)| id.parse::<u64>().is_ok())
            })
            .map(|line| {
                let mut fields = line.splitn(4, '\t');
                let id = fields
                    .next()
                    .context("missing module index")?
                    .parse::<u64>()
                    .context("invalid module index")?;
                let name = fields.next().context("missing module name")?;
                let argument = fields.next().unwrap_or_default();
                Ok(serde_json::json!({"index":id, "name":name, "argument":argument}))
            })
            .collect()
    }

    fn array<'a>(snapshot: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
        snapshot[key]
            .as_array()
            .with_context(|| format!("pactl activity snapshot has no {key} array"))
    }

    fn index(value: &Value) -> Option<u64> {
        value
            .as_u64()
            .or_else(|| value.as_str()?.parse().ok())
            .filter(|id| *id < u64::from(u32::MAX))
    }

    fn argument<'a>(module: &'a Value, key: &str) -> Option<&'a str> {
        module["argument"]
            .as_str()?
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .find_map(|word| {
                word.split_once('=')
                    .filter(|(name, _)| *name == key)
                    .map(|(_, value)| value)
            })
    }

    fn babel_stream(stream: &Value) -> bool {
        stream["properties"]["babel.owner"].as_str() == Some(OWNER)
            || stream["properties"]["application.id"].as_str() == Some(APP_ID)
    }

    fn is_monitor_of(source: &Value, sink: &Value) -> bool {
        index(&source["monitor_of_sink"]).is_some_and(|id| index(&sink["index"]) == Some(id))
            || index(&sink["monitor_source"]).is_some_and(|id| index(&source["index"]) == Some(id))
            // pactl 17 emits names (and calls the source's parent
            // "monitor_source") rather than numeric monitor indices.
            || source["monitor_source"].as_str().is_some_and(|name| !name.is_empty() && sink["name"].as_str() == Some(name))
            || sink["monitor_source"].as_str().is_some_and(|name| !name.is_empty() && source["name"].as_str() == Some(name))
            || source["monitor_of_sink_name"].as_str().is_some_and(|name| !name.is_empty() && sink["name"].as_str() == Some(name))
            || sink["monitor_source_name"].as_str().is_some_and(|name| !name.is_empty() && source["name"].as_str() == Some(name))
    }

    fn uncorked(stream: &Value) -> Result<bool> {
        match &stream["corked"] {
            Value::Bool(value) => Ok(!value),
            Value::String(value) if value == "no" || value == "false" => Ok(true),
            Value::String(value) if value == "yes" || value == "true" => Ok(false),
            _ => bail!("pactl stream has no valid corked state"),
        }
    }

    fn check_babel_target(stream: &Value, devices: &[Value], field: &str) -> Result<()> {
        if !babel_stream(stream) {
            return Ok(());
        }
        let target = stream["properties"]["babel.target"]
            .as_str()
            .context("Babel audio stream has no explicit target")?;
        let expected = devices
            .iter()
            .find(|device| device["name"].as_str() == Some(target))
            .and_then(|device| index(&device["index"]));
        let Some(actual) = index(&stream[field]) else {
            // A new stream can briefly be unlinked. It cannot carry audio to an
            // incorrect endpoint until a real endpoint index has been assigned.
            return Ok(());
        };
        ensure!(
            expected == Some(actual),
            "Babel audio stream was moved away from its configured target {target:?}; routing was suspended"
        );
        Ok(())
    }

    fn microphone_sources(
        sinks: &[Value],
        sources: &[Value],
        modules: &[Value],
        mic_device: &str,
    ) -> Result<HashSet<u64>> {
        let mic_bus = sinks
            .iter()
            .find(|sink| sink["name"].as_str() == Some(mic_device))
            .with_context(|| format!("virtual microphone bus {mic_device:?} is unavailable"))?;
        index(&mic_bus["index"]).context("virtual microphone bus has no index")?;
        let monitor = sources
            .iter()
            .find(|source| is_monitor_of(source, mic_bus))
            .context("virtual microphone bus has no monitor source")?;
        let monitor_name = monitor["name"]
            .as_str()
            .context("microphone bus monitor has no name")?;
        let monitor_id = index(&monitor["index"]).context("microphone bus monitor has no index")?;
        let mut mic_sources = HashSet::from([monitor_id]);
        for module in modules.iter().filter(|module| {
            module["name"].as_str() == Some("module-remap-source")
                && argument(module, "master") == Some(monitor_name)
        }) {
            let module_id = index(&module["index"]);
            let name = argument(module, "source_name");
            for source in sources.iter().filter(|source| {
                module_id.is_some_and(|id| index(&source["owner_module"]) == Some(id))
                    || name.is_some_and(|name| source["name"].as_str() == Some(name))
            }) {
                mic_sources.insert(
                    index(&source["index"]).context("remapped microphone source has no index")?,
                );
            }
        }
        Ok(mic_sources)
    }

    fn default_microphone_selected(
        sources: &[Value],
        microphone_sources: &HashSet<u64>,
        default_source: &str,
    ) -> bool {
        !default_source.is_empty()
            && sources.iter().any(|source| {
                source["name"].as_str() == Some(default_source)
                    && index(&source["index"]).is_some_and(|id| microphone_sources.contains(&id))
            })
    }

    fn speaker_sink(sinks: &[Value], sources: &[Value], speaker_device: &str) -> Result<u64> {
        let monitor = sources
            .iter()
            .find(|source| source["name"].as_str() == Some(speaker_device))
            .with_context(|| {
                format!("virtual speaker monitor {speaker_device:?} is unavailable")
            })?;
        index(&monitor["index"]).context("virtual speaker monitor has no index")?;
        let sink = sinks
            .iter()
            .find(|sink| is_monitor_of(monitor, sink))
            .context("configured speaker capture is not a sink monitor")?;
        index(&sink["index"]).context("virtual speaker sink has no index")
    }

    fn client_index(stream: &Value) -> Result<Option<u64>> {
        if !uncorked(stream)? {
            return Ok(None);
        }
        Ok(Some(
            index(&stream["index"]).context("audio stream has no index")?,
        ))
    }

    fn evaluate(
        snapshot: &Value,
        mic_device: &str,
        speaker_device: &str,
        default_source: Option<&str>,
    ) -> Result<UseSnapshot> {
        let sinks = array(snapshot, "sinks")?;
        let sources = array(snapshot, "sources")?;
        let modules = array(snapshot, "modules")?;
        let source_outputs = array(snapshot, "source_outputs")?;
        let sink_inputs = array(snapshot, "sink_inputs")?;
        let owned_modules: HashSet<u64> = modules
            .iter()
            .filter(|module| argument(module, "babel.owner") == Some(OWNER))
            .filter_map(|module| index(&module["index"]))
            .collect();
        let mut result = UseSnapshot::default();
        let mic_sources = match microphone_sources(sinks, sources, modules, mic_device) {
            Ok(sources) => sources,
            Err(error) => {
                result.microphone_error = Some(error.to_string());
                HashSet::new()
            }
        };
        result.microphone_default = default_source
            .is_some_and(|name| default_microphone_selected(sources, &mic_sources, name));
        let speaker_sink_id = match speaker_sink(sinks, sources, speaker_device) {
            Ok(id) => Some(id),
            Err(error) => {
                result.speaker_error = Some(error.to_string());
                None
            }
        };
        for stream in source_outputs {
            if let Err(error) = check_babel_target(stream, sources, "source") {
                // Capture of the virtual speaker is the output route. All other
                // Babel capture streams are the user's chosen physical mic.
                if stream["properties"]["babel.target"].as_str() == Some(speaker_device) {
                    result.speaker_error = Some(error.to_string());
                } else {
                    result.microphone_error = Some(error.to_string());
                }
            }
            if babel_stream(stream)
                || index(&stream["owner_module"]).is_some_and(|id| owned_modules.contains(&id))
            {
                continue;
            }
            if index(&stream["source"]).is_some_and(|id| mic_sources.contains(&id)) {
                match client_index(stream) {
                    Ok(Some(id)) => {
                        result.microphone_clients.insert(id);
                    }
                    Ok(None) => (),
                    Err(error) => result.microphone_error = Some(error.to_string()),
                }
            }
        }
        for stream in sink_inputs {
            if let Err(error) = check_babel_target(stream, sinks, "sink") {
                if stream["properties"]["babel.target"].as_str() == Some(mic_device) {
                    result.microphone_error = Some(error.to_string());
                } else {
                    result.speaker_error = Some(error.to_string());
                }
            }
            if babel_stream(stream)
                || index(&stream["owner_module"]).is_some_and(|id| owned_modules.contains(&id))
            {
                continue;
            }
            if speaker_sink_id.is_some() && index(&stream["sink"]) == speaker_sink_id {
                if let Some(id) = index(&stream["index"]) {
                    result.speaker_selected_clients.insert(id);
                }
                match client_index(stream) {
                    Ok(Some(id)) => {
                        result.speaker_clients.insert(id);
                    }
                    Ok(None) => (),
                    Err(error) => result.speaker_error = Some(error.to_string()),
                }
            }
        }
        if result.microphone_error.is_some() {
            result.microphone_clients.clear();
            result.microphone_default = false;
        }
        if result.speaker_error.is_some() {
            result.speaker_clients.clear();
            result.speaker_selected_clients.clear();
            result.speaker_default = false;
        }
        Ok(result)
    }

    fn parse_event(line: &str) -> Option<Event> {
        let removed = line.starts_with("Event 'remove' on ");
        let (_, target) = line.split_once(" on ")?;
        let (kind, id) = target.trim().split_once(" #")?;
        match (kind, removed) {
            ("source-output", true) => Some(Event::RemovedSourceOutput(id.parse().ok()?)),
            ("sink-input", true) => Some(Event::RemovedSinkInput(id.parse().ok()?)),
            ("source-output" | "sink-input" | "source" | "sink" | "module" | "server", _) => {
                Some(Event::Refresh)
            }
            // Ignore client events: each inspection creates its own pactl client.
            _ => None,
        }
    }

    async fn subscribe(events: mpsc::Sender<Event>, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() || events.is_closed() {
                return;
            }
            if let Ok(mut child) = Command::new("pactl")
                .arg("subscribe")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                if let Some(mut stdout) = child.stdout.take() {
                    let mut bytes = [0_u8; 1024];
                    let mut line = Vec::with_capacity(256);
                    let mut oversized = false;
                    loop {
                        let n = tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = events.closed() => break,
                            read = stdout.read(&mut bytes) => match read { Ok(0) | Err(_) => break, Ok(n) => n },
                        };
                        for byte in &bytes[..n] {
                            if *byte == b'\n' {
                                if !oversized
                                    && let Ok(text) = std::str::from_utf8(&line)
                                    && let Some(event) = parse_event(text)
                                {
                                    // Bounded backpressure preserves removals for short off/on
                                    // transitions; the periodic snapshot repairs subscription loss.
                                    tokio::select! {
                                        _ = cancel.cancelled() => { let _ = child.kill().await; return; }
                                        sent = events.send(event) => if sent.is_err() { let _ = child.kill().await; return; },
                                    }
                                }
                                line.clear();
                                oversized = false;
                            } else if line.len() < 1024 {
                                line.push(*byte);
                            } else {
                                oversized = true;
                            }
                        }
                    }
                }
                let _ = child.kill().await;
            }
            // Resynchronize after a subscription reconnect, including a server restart.
            let _ = events.try_send(Event::Refresh);
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = events.closed() => return,
                _ = tokio::time::sleep(REFRESH_INTERVAL) => (),
            }
        }
    }

    #[cfg(test)]
    mod tests;
}
