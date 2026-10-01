//! Small, typed HAL boundary. All FFI lives in the safe coreaudio HAL wrapper.
//! We use scalar/ID-vector properties and UID qualifiers, avoiding retained
//! CFString getters and their ownership ambiguity in a recurring monitor.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use coreaudio_hal::{
    AudioObject, MissingQualifier, PROCESS_INPUT_DEVICES, PROCESS_IS_RUNNING_INPUT,
    PROCESS_IS_RUNNING_OUTPUT, PROCESS_OUTPUT_DEVICES, PROCESS_PID, SYSTEM_TRANSLATE_UID_TO_DEVICE,
    System,
    property::{SYSTEM_DEFAULT_INPUT, SYSTEM_DEFAULT_OUTPUT},
};
use cpal::traits::DeviceTrait;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{Observation, ProcessUse, UseSnapshot, classify_processes};
use crate::audio::{DeviceDirection, native};

const REFRESH_INTERVAL: Duration = Duration::from_millis(200);
const RESOLVE_RETRY: Duration = Duration::from_secs(1);
const MAX_PROCESSES: usize = 4096;
const MAX_PROCESS_DEVICES: usize = 1024;

struct Target {
    setting: String,
    uid: Option<String>,
    error: Option<String>,
    retry_at: Instant,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            setting: String::new(),
            uid: None,
            error: None,
            retry_at: Instant::now(),
        }
    }
}

impl Target {
    fn update(&mut self, setting: &str, direction: DeviceDirection) {
        if self.setting == setting
            && (self.uid.is_some() || setting.is_empty() || Instant::now() < self.retry_at)
        {
            return;
        }
        self.setting = setting.to_owned();
        self.uid = None;
        self.error = None;
        if setting.is_empty() {
            return;
        }
        self.retry_at = Instant::now() + RESOLVE_RETRY;
        match native::resolve(setting, direction).and_then(|device| {
            Ok(device
                .id()
                .context("reading stable CoreAudio device UID")?
                .id()
                .to_owned())
        }) {
            Ok(uid) => self.uid = Some(uid),
            Err(error) => {
                self.error = Some(format!(
                    "CoreAudio virtual device is unavailable: {error:#}"
                ))
            }
        }
    }

    fn object_id(&self, system: &AudioObject<System>) -> Result<Option<u32>> {
        if let Some(error) = &self.error {
            anyhow::bail!("{error}");
        }
        let Some(uid) = &self.uid else {
            return Ok(None);
        };
        let id = system
            .get_property(SYSTEM_TRANSLATE_UID_TO_DEVICE.with_qualifier(uid.clone()))
            .context("resolving CoreAudio virtual-device UID")?;
        ensure!(id != 0, "CoreAudio virtual device is disconnected");
        Ok(Some(id))
    }
}

pub(super) fn run(
    mut mic_playback: watch::Receiver<String>,
    mut speaker_capture: watch::Receiver<String>,
    cancel: CancellationToken,
    updates: mpsc::Sender<Observation>,
) {
    let system = AudioObject::<System>::default();
    let mut microphone = Target::default();
    let mut speaker = Target::default();
    // Keep synchronous HAL queries on one worker, outside the audio callbacks
    // and async executor. No subprocess or new thread is created per snapshot.
    while !cancel.is_cancelled() && !updates.is_closed() {
        let started = Instant::now();
        let next_poll = started + REFRESH_INTERVAL;
        let mic_setting = mic_playback.borrow_and_update().clone();
        let speaker_setting = speaker_capture.borrow_and_update().clone();
        microphone.update(&mic_setting, DeviceDirection::Output);
        speaker.update(&speaker_setting, DeviceDirection::Input);
        let snapshot = match inspect(&system, &microphone, &speaker) {
            Ok(snapshot) => snapshot,
            Err(error) => UseSnapshot {
                error: Some(format!(
                    "CoreAudio could not identify virtual-device clients; macOS 14.2 or later is required: {error:#}"
                )),
                ..UseSnapshot::default()
            },
        };
        let _ = updates.try_send(Observation {
            started,
            microphone_setting: mic_setting,
            speaker_setting,
            snapshot,
        });
        std::thread::sleep(next_poll.saturating_duration_since(Instant::now()));
    }
}

fn inspect(
    system: &AudioObject<System>,
    microphone: &Target,
    speaker: &Target,
) -> Result<UseSnapshot> {
    let (microphone_device, mut microphone_error) = resolve_target(microphone, system);
    let (speaker_device, mut speaker_error) = resolve_target(speaker, system);
    if microphone_device.is_some() && microphone_device == speaker_device {
        return Ok(classify_processes(
            std::process::id(),
            microphone_device,
            speaker_device,
            &[],
        ));
    }
    if microphone_device.is_none() && speaker_device.is_none() {
        return Ok(UseSnapshot {
            microphone_error,
            speaker_error,
            ..UseSnapshot::default()
        });
    }
    // Process objects are supported from macOS 14.2. Older HALs return an
    // unsupported-property error. DeviceIsRunningSomewhere is never used:
    // it includes our own client and would leave the speaker self-activated.
    let processes = system
        .processes()
        .context("enumerating CoreAudio process objects")?;
    ensure!(
        processes.len() <= MAX_PROCESSES,
        "too many CoreAudio process objects"
    );
    let own_pid = std::process::id();
    let mut uses = Vec::with_capacity(processes.len());
    for process in processes {
        let pid = process
            .get_property(PROCESS_PID)
            .context("reading CoreAudio client PID")?;
        if pid <= 0 || pid as u32 == own_pid {
            continue;
        }
        let mut usage = ProcessUse {
            object_id: process.id(),
            pid: pid as u32,
            ..ProcessUse::default()
        };
        if microphone_device.is_some() && microphone_error.is_none() {
            match process
                .get_property(PROCESS_IS_RUNNING_INPUT)
                .and_then(|running| {
                    if running {
                        process
                            .get_property(PROCESS_INPUT_DEVICES)
                            .map(|devices| (true, devices))
                    } else {
                        Ok((false, Vec::new()))
                    }
                }) {
                Ok((running, devices)) if devices.len() <= MAX_PROCESS_DEVICES => {
                    usage.running_input = running;
                    usage.input_devices = devices;
                }
                Ok(_) => {
                    microphone_error = Some("Too many CoreAudio input devices for a client".into())
                }
                Err(error) => {
                    microphone_error = Some(format!(
                        "CoreAudio could not identify input clients: {error}"
                    ))
                }
            }
        }
        if speaker_device.is_some() && speaker_error.is_none() {
            match process
                .get_property(PROCESS_IS_RUNNING_OUTPUT)
                .and_then(|running| {
                    if running {
                        process
                            .get_property(PROCESS_OUTPUT_DEVICES)
                            .map(|devices| (true, devices))
                    } else {
                        Ok((false, Vec::new()))
                    }
                }) {
                Ok((running, devices)) if devices.len() <= MAX_PROCESS_DEVICES => {
                    usage.running_output = running;
                    usage.output_devices = devices;
                }
                Ok(_) => {
                    speaker_error = Some("Too many CoreAudio output devices for a client".into())
                }
                Err(error) => {
                    speaker_error = Some(format!(
                        "CoreAudio could not identify output clients: {error}"
                    ))
                }
            }
        }
        uses.push(usage);
    }
    let mut snapshot = classify_processes(own_pid, microphone_device, speaker_device, &uses);
    if microphone_device.is_some() && microphone_error.is_none() {
        match system.get_property(SYSTEM_DEFAULT_INPUT) {
            Ok(id) => snapshot.select_default_microphone(Some(id)),
            Err(error) => {
                microphone_error = Some(format!(
                    "CoreAudio could not inspect the system microphone selection: {error}"
                ))
            }
        }
    }
    if speaker_device.is_some() && speaker_error.is_none() {
        match system.get_property(SYSTEM_DEFAULT_OUTPUT) {
            Ok(id) => snapshot.select_default_speaker(Some(id)),
            Err(error) => {
                speaker_error = Some(format!(
                    "CoreAudio could not inspect the system speaker selection: {error}"
                ))
            }
        }
    }
    snapshot.microphone_error = microphone_error;
    snapshot.speaker_error = speaker_error;
    Ok(snapshot)
}

fn resolve_target(target: &Target, system: &AudioObject<System>) -> (Option<u32>, Option<String>) {
    match target.object_id(system) {
        Ok(id) => (id, None),
        Err(error) => (None, Some(format!("{error:#}"))),
    }
}
