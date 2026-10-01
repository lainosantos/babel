//! CoreAudio process clients of the two configured virtual cables (macOS 14.2+).
//! The selected system input also authorizes microphone routing. No hardware
//! streams or audio taps are opened here; speaker capture still requires a
//! client, while the selected system output authorizes pending translated audio.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use super::EndpointUse;

#[cfg(target_os = "macos")]
mod hal;

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(target_os = "macos")]
static HAL_WORKER: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[cfg(target_os = "macos")]
pub(super) async fn run(
    mic_playback: tokio::sync::watch::Receiver<String>,
    speaker_capture: tokio::sync::watch::Receiver<String>,
    cancel: tokio_util::sync::CancellationToken,
    state: tokio::sync::watch::Sender<EndpointUse>,
) {
    // Keep the permit inside the actual HAL worker. A wedged call cannot create
    // overlapping monitor workers even if a new audio session is started.
    if HAL_WORKER.available_permits() == 0 {
        UseTracker::default().fail(
            &state,
            "Waiting for the previous CoreAudio device-use query to stop",
        );
    }
    let permit = tokio::select! {
        _ = cancel.cancelled() => return,
        _ = state.closed() => return,
        permit = HAL_WORKER.acquire() => match permit { Ok(permit) => permit, Err(_) => return },
    };
    let worker_cancel = cancel.child_token();
    let _cancel_worker = worker_cancel.clone().drop_guard();
    let (updates, observations) = tokio::sync::mpsc::channel(1);
    let worker_mic = mic_playback.clone();
    let worker_speaker = speaker_capture.clone();
    // Only one blocking worker is ever started by this monitor. If it panics or
    // exits, the closed observation channel makes the supervisor fail closed.
    let _worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hal::run(worker_mic, worker_speaker, worker_cancel, updates);
    });
    supervise(mic_playback, speaker_capture, cancel, state, observations).await;
}

async fn supervise(
    mut mic_playback: tokio::sync::watch::Receiver<String>,
    mut speaker_capture: tokio::sync::watch::Receiver<String>,
    cancel: tokio_util::sync::CancellationToken,
    state: tokio::sync::watch::Sender<EndpointUse>,
    mut observations: tokio::sync::mpsc::Receiver<Observation>,
) {
    let mut tracker = UseTracker::default();
    let mut deadline = tokio::time::Instant::now() + SNAPSHOT_TIMEOUT;
    let mut mic_watch_open = true;
    let mut speaker_watch_open = true;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = state.closed() => break,
            changed = mic_playback.changed(), if mic_watch_open => {
                if changed.is_ok() {
                    mic_playback.borrow_and_update();
                    tracker.invalidate_selection(&state, true);
                } else { mic_watch_open = false; }
            }
            changed = speaker_capture.changed(), if speaker_watch_open => {
                if changed.is_ok() {
                    speaker_capture.borrow_and_update();
                    tracker.invalidate_selection(&state, false);
                } else { speaker_watch_open = false; }
            }
            observation = observations.recv() => {
                let Some(observation) = observation else {
                    tracker.fail(&state, "CoreAudio device-use monitor stopped");
                    break;
                };
                let mic = mic_playback.borrow().clone();
                let speaker = speaker_capture.borrow().clone();
                let now = Instant::now();
                let remaining = SNAPSHOT_TIMEOUT.saturating_sub(now.saturating_duration_since(observation.started));
                tracker.publish(&state, observation.validate(now, &mic, &speaker));
                // Age starts before the HAL query, not after delivery. A slow
                // query or a queued snapshot cannot extend its own validity.
                deadline = tokio::time::Instant::now()
                    + if remaining.is_zero() { SNAPSHOT_TIMEOUT } else { remaining };
            }
            _ = tokio::time::sleep_until(deadline) => {
                tracker.fail(&state, "CoreAudio device-use query timed out; audio routing is suspended");
                deadline = tokio::time::Instant::now() + SNAPSHOT_TIMEOUT;
            }
        }
    }
}

struct Observation {
    started: Instant,
    microphone_setting: String,
    speaker_setting: String,
    snapshot: UseSnapshot,
}

impl Observation {
    fn validate(mut self, now: Instant, microphone: &str, speaker: &str) -> UseSnapshot {
        if now.saturating_duration_since(self.started) >= SNAPSHOT_TIMEOUT {
            return UseSnapshot {
                error: Some(
                    "CoreAudio device-use query timed out; audio routing is suspended".into(),
                ),
                ..UseSnapshot::default()
            };
        }
        if self.microphone_setting != microphone {
            self.snapshot.microphone_clients.clear();
            self.snapshot.microphone_default = false;
            self.snapshot.microphone_device = None;
            self.snapshot.microphone_error = None;
        }
        if self.speaker_setting != speaker {
            self.snapshot.speaker_clients.clear();
            self.snapshot.speaker_default = false;
            self.snapshot.speaker_device = None;
            self.snapshot.speaker_error = None;
        }
        self.snapshot
    }
}

#[derive(Clone, Debug, Default)]
struct ProcessUse {
    object_id: u32,
    pid: u32,
    running_input: bool,
    running_output: bool,
    input_devices: Vec<u32>,
    output_devices: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
struct UseSnapshot {
    microphone_device: Option<u32>,
    speaker_device: Option<u32>,
    microphone_default: bool,
    speaker_default: bool,
    microphone_clients: BTreeSet<(u32, u32)>,
    speaker_clients: BTreeSet<(u32, u32)>,
    microphone_error: Option<String>,
    speaker_error: Option<String>,
    error: Option<String>,
}

impl UseSnapshot {
    fn select_default_microphone(&mut self, default_input: Option<u32>) {
        self.microphone_default = self
            .microphone_device
            .is_some_and(|id| id != 0 && Some(id) == default_input);
    }

    fn select_default_speaker(&mut self, default_output: Option<u32>) {
        self.speaker_default = self
            .speaker_device
            .is_some_and(|id| id != 0 && Some(id) == default_output);
    }
}

fn classify_processes(
    babel_pid: u32,
    microphone_device: Option<u32>,
    speaker_device: Option<u32>,
    processes: &[ProcessUse],
) -> UseSnapshot {
    let mut result = UseSnapshot {
        microphone_device,
        speaker_device,
        ..UseSnapshot::default()
    };
    if microphone_device.is_some() && microphone_device == speaker_device {
        result.error =
            Some("CoreAudio virtual microphone and speaker must use independent devices".into());
        return result;
    }
    for process in processes {
        if process.pid == 0 || process.pid == babel_pid {
            continue;
        }
        let identity = (process.object_id, process.pid);
        // A duplex app can capture a physical mic while playing to Babel. Match
        // the scoped device list, not merely an overall process-running flag.
        if process.running_input
            && microphone_device.is_some_and(|id| process.input_devices.contains(&id))
        {
            result.microphone_clients.insert(identity);
        }
        if process.running_output
            && speaker_device.is_some_and(|id| process.output_devices.contains(&id))
        {
            result.speaker_clients.insert(identity);
        }
    }
    result
}

#[derive(Default)]
struct UseTracker {
    previous: UseSnapshot,
}

impl UseTracker {
    fn fail(&mut self, state: &tokio::sync::watch::Sender<EndpointUse>, message: &str) {
        self.publish(
            state,
            UseSnapshot {
                error: Some(message.into()),
                ..UseSnapshot::default()
            },
        );
    }

    fn invalidate_selection(
        &mut self,
        state: &tokio::sync::watch::Sender<EndpointUse>,
        microphone: bool,
    ) {
        let mut next = self.previous.clone();
        if microphone {
            next.microphone_clients.clear();
            next.microphone_default = false;
            next.microphone_device = None;
            next.microphone_error = None;
        } else {
            next.speaker_clients.clear();
            next.speaker_default = false;
            next.speaker_device = None;
            next.speaker_error = None;
        }
        self.publish(state, next);
    }

    fn publish(&mut self, state: &tokio::sync::watch::Sender<EndpointUse>, mut next: UseSnapshot) {
        if next.error.is_some() || next.microphone_error.is_some() {
            next.microphone_clients.clear();
            next.microphone_default = false;
        }
        if next.error.is_some() || next.speaker_error.is_some() {
            next.speaker_clients.clear();
            next.speaker_default = false;
        }
        let microphone = next.microphone_default || !next.microphone_clients.is_empty();
        let speaker = !next.speaker_clients.is_empty();
        let speaker_selected = next.speaker_default || speaker;
        let microphone_replaced = self.previous.microphone_device != next.microphone_device
            || (!(self.previous.microphone_default && next.microphone_default)
                && !self
                    .previous
                    .microphone_clients
                    .is_subset(&next.microphone_clients));
        let speaker_replaced = self.previous.speaker_device != next.speaker_device
            || !self
                .previous
                .speaker_clients
                .is_subset(&next.speaker_clients);
        let speaker_selection_replaced = self.previous.speaker_device != next.speaker_device
            || (!(self.previous.speaker_default && next.speaker_default)
                && !self
                    .previous
                    .speaker_clients
                    .is_subset(&next.speaker_clients));
        state.send_if_modified(|current| {
            let mic_changed = current.microphone != microphone
                || microphone_replaced
                || current.microphone_error != next.microphone_error;
            let speaker_changed = current.speaker != speaker
                || speaker_replaced
                || current.speaker_error != next.speaker_error;
            let error_changed = current.error != next.error;
            let speaker_selection_changed = current.speaker_selected != speaker_selected
                || speaker_selection_replaced
                || current.speaker_error != next.speaker_error
                || error_changed;
            if mic_changed {
                current.microphone_epoch = current.microphone_epoch.wrapping_add(1);
            }
            if speaker_changed {
                current.speaker_epoch = current.speaker_epoch.wrapping_add(1);
            }
            if speaker_selection_changed {
                current.speaker_selection_epoch = current.speaker_selection_epoch.wrapping_add(1);
            }
            current.microphone = microphone;
            current.speaker = speaker;
            current.speaker_selected = speaker_selected;
            current.microphone_error.clone_from(&next.microphone_error);
            current.speaker_error.clone_from(&next.speaker_error);
            current.error.clone_from(&next.error);
            mic_changed || speaker_changed || speaker_selection_changed || error_changed
        });
        self.previous = next;
    }
}

#[cfg(test)]
mod tests;
