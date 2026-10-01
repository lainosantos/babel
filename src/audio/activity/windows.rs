//! WASAPI control-thread activity inspection. No stream is opened to inspect
//! usage. All COM objects stay on one MTA thread and all FFI is in `wasapi`.
//!
//! A fresh manager/enumerator is acquired on every poll, while discovered
//! session controls and state notifications are retained. wasapi 0.24 does not
//! expose OnSessionCreated: an OS-omitted new session remains conservatively
//! inactive until discovered. This is not an exclusive-mode/ASIO/KS detector.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use cpal::traits::DeviceTrait;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use wasapi::{
    AudioSessionControl, DeviceEnumerator, DeviceState, EventCallbacks, EventRegistration,
    SessionState,
};

static WORKER_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

use super::{
    EndpointUse,
    windows_policy::{self as policy, ActivitySignal, Direction, Endpoint, Pair, SessionActivity},
};
use crate::audio::DeviceDirection;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const CATALOG_INTERVAL: Duration = Duration::from_secs(1);
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ENDPOINTS: u32 = 256;
const MAX_SESSIONS: i32 = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteObservation {
    pair: Pair,
    active: bool,
    microphone_default: bool,
    speaker_default: bool,
    callback_epoch: u64,
}

struct Observation {
    microphone_selection: String,
    speaker_selection: String,
    microphone: Result<RouteObservation, String>,
    speaker: Result<RouteObservation, String>,
    error: Option<String>,
    finished: Instant,
}

/// The async supervisor can close both gates if a Windows RPC stalls. It never
/// creates replacement blocking threads while the previous call is outstanding.
pub(super) async fn run(
    mut mic_playback: watch::Receiver<String>,
    mut speaker_capture: watch::Receiver<String>,
    cancel: CancellationToken,
    state: watch::Sender<EndpointUse>,
) {
    let slot = match WORKER_SLOT.try_acquire() {
        Ok(slot) => slot,
        Err(_) => {
            publish(&state, None, None, Some("waiting for the previous Windows audio inspection to finish; routing remains suspended".into()), false, false, false);
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = state.closed() => return,
                slot = WORKER_SLOT.acquire() => match slot { Ok(slot) => slot, Err(_) => return },
            }
        }
    };
    let (sender, mut observations) = mpsc::channel(1);
    let worker_cancel = cancel.child_token();
    let _cancel_on_drop = worker_cancel.clone().drop_guard();
    let thread_cancel = worker_cancel.clone();
    let mic = mic_playback.clone();
    let speaker = speaker_capture.clone();
    let worker = thread::Builder::new()
        .name("babel-wasapi-activity".into())
        .spawn(move || {
            let _slot = slot;
            worker(mic, speaker, thread_cancel, sender);
        });
    let Ok(worker) = worker else {
        publish(
            &state,
            None,
            None,
            Some("could not start the Windows audio activity monitor".into()),
            false,
            false,
            false,
        );
        return;
    };
    let mut last_microphone = None;
    let mut last_speaker = None;
    let mut last_observation = Instant::now();
    let mut watchdog = tokio::time::interval(POLL_INTERVAL);
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut mic_open = true;
    let mut speaker_open = true;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = state.closed() => break,
            changed = mic_playback.changed(), if mic_open => {
                mic_open = changed.is_ok();
                invalidate(&state, true);
                last_microphone = None;
                worker.thread().unpark();
            }
            changed = speaker_capture.changed(), if speaker_open => {
                speaker_open = changed.is_ok();
                invalidate(&state, false);
                last_speaker = None;
                worker.thread().unpark();
            }
            observation = observations.recv() => {
                let Some(observation) = observation else {
                    publish(&state, None, None, Some("the Windows audio activity monitor stopped; routing remains suspended".into()), false, false, false);
                    break;
                };
                if observation.microphone_selection != *mic_playback.borrow_and_update()
                    || observation.speaker_selection != *speaker_capture.borrow_and_update()
                {
                    continue;
                }
                last_observation = observation.finished;
                if last_observation.elapsed() > INSPECTION_TIMEOUT {
                    publish(&state, None, None, Some("Windows audio activity inspection timed out; routing remains suspended".into()), false, false, false);
                    continue;
                }
                let microphone_changed = changed_mapping(&last_microphone, &observation.microphone);
                let speaker_changed = changed_mapping(&last_speaker, &observation.speaker);
                let speaker_selection_changed = changed_speaker_selection(&last_speaker, &observation.speaker);
                publish(&state, Some(&observation.microphone), Some(&observation.speaker), observation.error, microphone_changed, speaker_changed, speaker_selection_changed);
                last_microphone = observation.microphone.ok();
                last_speaker = observation.speaker.ok();
            }
            _ = watchdog.tick() => {
                if last_observation.elapsed() > INSPECTION_TIMEOUT {
                    publish(&state, None, None, Some("Windows audio activity inspection timed out; routing remains suspended".into()), false, false, false);
                }
            }
        }
    }
    worker_cancel.cancel();
    worker.thread().unpark();
    // Do not block Tokio shutdown waiting for an uninterruptible OS RPC. There
    // is just one worker; it exits after its current call and owns no audio I/O.
    drop(observations);
    if worker.is_finished() {
        let _ = worker.join();
    }
    state.send_modify(|current| {
        if current.microphone {
            current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
        }
        if current.speaker {
            current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
        }
        if current.speaker_selected {
            current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
        }
        current.microphone = false;
        current.speaker = false;
        current.speaker_selected = false;
    });
}

fn changed_mapping(
    previous: &Option<RouteObservation>,
    current: &Result<RouteObservation, String>,
) -> bool {
    current.as_ref().is_ok_and(|current| {
        previous.as_ref().is_none_or(|previous| {
            previous.pair != current.pair
                || policy::client_epoch_changed(
                    previous.microphone_default,
                    current.microphone_default,
                    previous.callback_epoch,
                    current.callback_epoch,
                )
        })
    })
}

fn changed_speaker_selection(
    previous: &Option<RouteObservation>,
    current: &Result<RouteObservation, String>,
) -> bool {
    current.as_ref().is_ok_and(|current| {
        previous.as_ref().is_none_or(|previous| {
            previous.pair != current.pair
                || policy::client_epoch_changed(
                    previous.speaker_default,
                    current.speaker_default,
                    previous.callback_epoch,
                    current.callback_epoch,
                )
        })
    })
}

fn publish(
    state: &watch::Sender<EndpointUse>,
    microphone: Option<&Result<RouteObservation, String>>,
    speaker: Option<&Result<RouteObservation, String>>,
    error: Option<String>,
    microphone_changed: bool,
    speaker_changed: bool,
    speaker_selection_changed: bool,
) {
    let mic_active = error.is_none()
        && microphone.is_some_and(|result| result.as_ref().is_ok_and(|route| route.active));
    let speaker_active = error.is_none()
        && speaker.is_some_and(|result| result.as_ref().is_ok_and(|route| route.active));
    let speaker_selected = error.is_none()
        && speaker.is_some_and(|result| {
            result
                .as_ref()
                .is_ok_and(|route| route.active || route.speaker_default)
        });
    let mic_error = microphone.and_then(|value| value.as_ref().err()).cloned();
    let speaker_error = speaker.and_then(|value| value.as_ref().err()).cloned();
    state.send_if_modified(|current| {
        let common = current.error != error;
        let mic_changed = microphone_changed
            || common
            || current.microphone != mic_active
            || current.microphone_error != mic_error;
        let speaker_changed = speaker_changed
            || common
            || current.speaker != speaker_active
            || current.speaker_error != speaker_error;
        let speaker_selection_changed = speaker_selection_changed
            || common
            || current.speaker_selected != speaker_selected
            || current.speaker_error != speaker_error;
        if mic_changed {
            current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
        }
        if speaker_changed {
            current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
        }
        if speaker_selection_changed {
            current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
        }
        current.microphone = mic_active;
        current.speaker = speaker_active;
        current.speaker_selected = speaker_selected;
        current.microphone_error = mic_error;
        current.speaker_error = speaker_error;
        current.error = error;
        mic_changed || speaker_changed || speaker_selection_changed
    });
}

fn invalidate(state: &watch::Sender<EndpointUse>, microphone: bool) {
    state.send_modify(|current| {
        if microphone {
            current.microphone = false;
            current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
        } else {
            current.speaker = false;
            current.speaker_selected = false;
            current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
            current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
        }
    });
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

fn worker(
    mic: watch::Receiver<String>,
    speaker: watch::Receiver<String>,
    cancel: CancellationToken,
    sender: mpsc::Sender<Observation>,
) {
    let initialized = wasapi::initialize_mta().ok();
    if let Err(error) = initialized {
        let _ = sender.blocking_send(Observation {
            microphone_selection: mic.borrow().clone(),
            speaker_selection: speaker.borrow().clone(),
            microphone: Err("Windows microphone activity inspection is unavailable".into()),
            speaker: Err("Windows speaker activity inspection is unavailable".into()),
            error: Some(format!(
                "could not initialize Windows audio inspection: {error}"
            )),
            finished: Instant::now(),
        });
        return;
    }
    let _apartment = Apartment;
    let mut mic_observer = Observer::default();
    let mut speaker_observer = Observer::default();
    let mut catalog: Vec<Endpoint> = Vec::new();
    let mut catalog_at = None;
    let mut selections = (String::new(), String::new());
    while !cancel.is_cancelled() && !sender.is_closed() {
        let mic_selection = mic.borrow().clone();
        let speaker_selection = speaker.borrow().clone();
        let requested = (mic_selection.clone(), speaker_selection.clone());
        let inspection_started = Instant::now();
        let inspected = inspect(
            &requested,
            &selections,
            &mut catalog,
            &mut catalog_at,
            &mut mic_observer,
            &mut speaker_observer,
        )
        .and_then(|snapshot| {
            ensure!(
                inspection_started.elapsed() <= INSPECTION_TIMEOUT,
                "Windows audio activity inspection timed out; its stale result was discarded"
            );
            Ok(snapshot)
        });
        selections = requested;
        let (microphone, speaker, error) = match inspected {
            Ok((microphone, speaker)) => (microphone, speaker, None),
            Err(error) => {
                mic_observer.clear();
                speaker_observer.clear();
                catalog_at = None;
                (
                    Err("Windows microphone activity inspection is unavailable".into()),
                    Err("Windows speaker activity inspection is unavailable".into()),
                    Some(format!(
                        "could not inspect Windows audio activity: {error:#}"
                    )),
                )
            }
        };
        if sender
            .blocking_send(Observation {
                microphone_selection: mic_selection,
                speaker_selection,
                microphone,
                speaker,
                error,
                finished: Instant::now(),
            })
            .is_err()
        {
            break;
        }
        thread::park_timeout(POLL_INTERVAL);
    }
}

type RouteResult = Result<RouteObservation, String>;

fn inspect(
    requested: &(String, String),
    previous: &(String, String),
    catalog: &mut Vec<Endpoint>,
    catalog_at: &mut Option<Instant>,
    mic_observer: &mut Observer,
    speaker_observer: &mut Observer,
) -> Result<(RouteResult, RouteResult)> {
    let enumerator = DeviceEnumerator::new().context("opening WASAPI device enumeration")?;
    if requested != previous || catalog_at.is_none_or(|at| at.elapsed() >= CATALOG_INTERVAL) {
        *catalog = endpoints(&enumerator)?;
        *catalog_at = Some(Instant::now());
    }
    let microphone = resolve_pair(catalog, &requested.0, Direction::Render);
    let speaker = resolve_pair(catalog, &requested.1, Direction::Capture);
    if let (Ok(mic), Ok(speaker)) = (&microphone, &speaker)
        && let Err(error) = policy::independent(mic, speaker)
    {
        mic_observer.clear();
        speaker_observer.clear();
        return Ok((Err(error.to_string()), Err(error.to_string())));
    }
    Ok((
        inspect_route(&enumerator, microphone, mic_observer, true),
        inspect_route(&enumerator, speaker, speaker_observer, false),
    ))
}

fn resolve_pair(catalog: &[Endpoint], selection: &str, direction: Direction) -> Result<Pair> {
    let cpal_direction = match direction {
        Direction::Render => DeviceDirection::Output,
        Direction::Capture => DeviceDirection::Input,
    };
    let device = crate::audio::native::resolve(selection, cpal_direction)?;
    let id = device
        .id()
        .context("reading the stable Windows endpoint ID")?;
    policy::pair(catalog, id.id(), direction)
}

fn endpoints(enumerator: &DeviceEnumerator) -> Result<Vec<Endpoint>> {
    let mut result = Vec::new();
    for (direction, native) in [
        (Direction::Capture, wasapi::Direction::Capture),
        (Direction::Render, wasapi::Direction::Render),
    ] {
        let devices = enumerator
            .get_device_collection(&native)
            .context("enumerating Windows audio endpoints")?;
        let count = devices
            .get_nbr_devices()
            .context("counting Windows audio endpoints")?;
        ensure!(
            count <= MAX_ENDPOINTS,
            "Windows audio endpoint count exceeds the inspection limit"
        );
        for index in 0..count {
            let device = devices
                .get_device_at_index(index)
                .context("reading a Windows audio endpoint")?;
            let id = device
                .get_id()
                .context("reading Windows audio endpoint identity")?;
            // PKEY_Device_DeviceDesc + PKEY_DeviceInterface_FriendlyName are
            // supplied by the driver. Do not substitute get_friendlyname():
            // that endpoint display name can be changed in Windows Settings.
            // Babel's four role descriptions are fixed in BabelAudio.inf; the
            // policy requires a unique opposite endpoint from the same cable.
            let description = device
                .get_description()
                .context("reading Windows audio driver description")?;
            let adapter = device
                .get_interface_friendlyname()
                .context("reading Windows audio adapter identity")?;
            ensure!(
                [id.len(), description.len(), adapter.len()]
                    .into_iter()
                    .all(|length| length <= 4096),
                "Windows audio metadata exceeds the inspection limit"
            );
            result.push(Endpoint {
                id,
                direction,
                description,
                adapter,
            });
        }
    }
    Ok(result)
}

fn inspect_route(
    enumerator: &DeviceEnumerator,
    pair: Result<Pair>,
    observer: &mut Observer,
    microphone: bool,
) -> RouteResult {
    let result = pair
        .and_then(|pair| observer.inspect(enumerator, pair))
        .map(|mut route| {
            // Windows permits separate defaults for calls and other apps. An
            // unavailable role grants no authorization; external sessions are
            // still inspected above. Speaker selection does not start capture.
            let direction = if microphone {
                wasapi::Direction::Capture
            } else {
                wasapi::Direction::Render
            };
            let defaults: Vec<String> = [
                wasapi::Role::Console,
                wasapi::Role::Multimedia,
                wasapi::Role::Communications,
            ]
            .iter()
            .filter_map(|role| {
                enumerator
                    .get_default_device_for_role(&direction, role)
                    .ok()
            })
            .filter_map(|device| device.get_id().ok())
            .collect();
            if microphone {
                route.microphone_default =
                    policy::microphone_requested(&route.pair, &defaults, false);
                route.active |= route.microphone_default;
            } else {
                route.speaker_default = policy::speaker_selected(&route.pair, &defaults, false);
            }
            route
        });
    if result.is_err() {
        observer.clear();
    }
    result.map_err(|error| format!("Windows virtual-device activity: {error:#}"))
}

struct Session {
    control: AudioSessionControl,
    _registration: EventRegistration,
    activity: Arc<SessionActivity>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.activity.update(false);
    }
}
#[derive(Default)]
struct Observer {
    pair: Option<Pair>,
    sessions: HashMap<String, Session>,
    signal: Arc<ActivitySignal>,
}
impl Observer {
    fn clear(&mut self) {
        self.sessions.clear();
        self.signal = Arc::new(ActivitySignal::default());
        self.pair = None;
    }
    fn inspect(&mut self, enumerator: &DeviceEnumerator, pair: Pair) -> Result<RouteObservation> {
        if self.pair.as_ref() != Some(&pair) {
            self.clear();
            self.pair = Some(pair.clone());
        }
        let selected = enumerator
            .get_device(&pair.selected)
            .context("opening the selected virtual endpoint")?;
        let opposite = enumerator
            .get_device(&pair.opposite)
            .context("opening the opposite virtual cable endpoint")?;
        ensure!(
            selected.get_state()? == DeviceState::Active
                && opposite.get_state()? == DeviceState::Active,
            "a Windows virtual cable endpoint is disconnected or disabled"
        );
        // Do not retain a stale session-enumerator snapshot between polls.
        let manager = opposite
            .get_iaudiosessionmanager()
            .context("opening WASAPI session inspection")?;
        let sessions = manager
            .get_audiosessionenumerator()
            .context("enumerating WASAPI sessions")?;
        let count = sessions.get_count().context("counting WASAPI sessions")?;
        ensure!(
            (0..=MAX_SESSIONS).contains(&count),
            "Windows audio session count exceeds the inspection limit"
        );
        let mut current_ids = HashSet::new();
        for index in 0..count {
            let control = sessions
                .get_session(index)
                .context("reading a WASAPI session")?;
            if control.get_state()? == SessionState::Expired {
                continue;
            }
            if !policy::external_process(
                control
                    .get_process_id()
                    .context("identifying the WASAPI session owner")?,
                std::process::id(),
            ) {
                continue;
            }
            let id = control
                .get_session_instance_identifier()
                .context("reading the WASAPI session identity")?;
            ensure!(
                !id.is_empty() && id.len() <= 4096,
                "invalid WASAPI session identity"
            );
            current_ids.insert(id.clone());
            if self.sessions.contains_key(&id) {
                continue;
            }
            let activity = SessionActivity::new(self.signal.clone())?;
            let mut callbacks = EventCallbacks::new();
            let flag = activity.clone();
            callbacks.set_state_callback(move |state| flag.update(state == SessionState::Active));
            let flag = activity.clone();
            callbacks.set_disconnected_callback(move |_| flag.update(false));
            let registration = control
                .register_session_notification(callbacks)
                .context("watching WASAPI session state")?;
            self.sessions.insert(
                id,
                Session {
                    control,
                    _registration: registration,
                    activity,
                },
            );
        }
        policy::retain_current_sessions(&mut self.sessions, &current_ids);
        let mut expired = Vec::new();
        for (id, session) in &self.sessions {
            let before = session.activity.changes.load(Ordering::Acquire);
            let state = session
                .control
                .get_state()
                .context("refreshing WASAPI session state")?;
            if session.activity.changes.load(Ordering::Acquire) == before {
                session.activity.update(state == SessionState::Active);
            }
            if state == SessionState::Expired {
                expired.push(id.clone());
            }
        }
        for id in expired {
            self.sessions.remove(&id);
        }
        Ok(RouteObservation {
            pair,
            // Only controls still present on this endpoint authorize audio.
            // A late callback owned by a removed control cannot reopen it.
            active: self
                .sessions
                .values()
                .any(|session| session.activity.active()),
            microphone_default: false,
            speaker_default: false,
            callback_epoch: self.signal.epoch(),
        })
    }
}
