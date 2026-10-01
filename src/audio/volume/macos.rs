//! Hardware volume control stays on Tokio's blocking control workers. The HAL
//! plug-in's speaker controls are metadata; original PCM is never attenuated.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result, ensure};
use coreaudio_hal::{
    AudioObject, DEVICE_OUTPUT_MUTE, DEVICE_OUTPUT_VOLUME_SCALAR, Device, MissingElement,
    MissingQualifier, STARTING_CHANNEL, STREAM_PHYSICAL_FORMAT, SYSTEM_TRANSLATE_UID_TO_DEVICE,
    Scope, System,
};
use cpal::traits::DeviceTrait;
use tokio_util::sync::CancellationToken;

use super::{Snapshot, VolumeState};
use crate::audio::{
    DeviceDirection, native,
    native_ids::{BABEL_MICROPHONE_UID, BABEL_SPEAKER_UID},
};

// Devices without a master control expose channel controls instead. Preserve
// their relative balance, including a trip through zero, without touching DSP.
type ChannelBalances = BTreeMap<String, Vec<(u32, f32)>>;
static BALANCES: OnceLock<Mutex<ChannelBalances>> = OnceLock::new();
const MAX_CHANNELS: u32 = 32;
const MAX_BALANCES: usize = 64;

struct Endpoint {
    object: AudioObject<Device>,
    uid: String,
}

fn resolve(setting: &str, direction: DeviceDirection) -> Result<Endpoint> {
    let device = native::resolve(setting, direction)?;
    let uid = device
        .id()
        .context("reading CoreAudio volume device UID")?
        .id()
        .to_owned();
    let id = AudioObject::<System>::default()
        .get_property(SYSTEM_TRANSLATE_UID_TO_DEVICE.with_qualifier(uid.clone()))
        .context("resolving CoreAudio volume device")?;
    ensure!(id != 0, "CoreAudio volume device is disconnected");
    Ok(Endpoint {
        object: id.into(),
        uid,
    })
}

fn output_channels(object: &AudioObject<Device>) -> Result<Vec<u32>> {
    let streams = object
        .streams_with_scope(Scope::Output)
        .context("reading CoreAudio output streams for channel volume")?;
    ensure!(
        !streams.is_empty() && streams.len() <= MAX_CHANNELS as usize,
        "This output has an unsupported number of audio streams"
    );
    let mut channels = BTreeSet::new();
    for stream in streams {
        let first = stream
            .get_property(STARTING_CHANNEL)
            .context("reading the first output channel of a CoreAudio stream")?;
        let count = stream
            .get_property(STREAM_PHYSICAL_FORMAT)
            .context("reading CoreAudio output channel count")?
            .channels_per_frame();
        add_channel_range(&mut channels, first, count)?;
    }
    Ok(channels.into_iter().collect())
}

fn add_channel_range(channels: &mut BTreeSet<u32>, first: u32, count: u32) -> Result<()> {
    let end = first
        .checked_add(count)
        .context("CoreAudio returned an invalid output channel range")?;
    ensure!(
        first > 0 && count > 0 && end <= MAX_CHANNELS + 1,
        "Channel volume control requires at most 32 output channels when there is no master control"
    );
    ensure!(
        (first..end).all(|channel| !channels.contains(&channel)),
        "CoreAudio returned overlapping output channel ranges"
    );
    channels.extend(first..end);
    Ok(())
}

struct Controls {
    volumes: Vec<(u32, f32)>,
    mutes: Vec<(u32, bool)>,
    writable: bool,
}

impl Controls {
    fn read(endpoint: &Endpoint) -> Result<Self> {
        let object = &endpoint.object;
        let mut volumes = Vec::new();
        if let Ok(level) = object.get_property(DEVICE_OUTPUT_VOLUME_SCALAR.for_element(0)) {
            volumes.push((0, level));
        } else {
            ensure!(
                endpoint.uid != BABEL_SPEAKER_UID,
                "The installed Babel Audio driver has no output volume controls"
            );
            // Require every actual output channel, rather than silently
            // controlling only the subset with readable volume properties.
            for channel in output_channels(object)? {
                let level = object
                    .get_property(DEVICE_OUTPUT_VOLUME_SCALAR.for_element(channel))
                    .context("This output does not expose volume controls for every channel")?;
                volumes.push((channel, level));
            }
        }
        ensure!(
            !volumes.is_empty(),
            "This output has no readable hardware volume control"
        );
        ensure!(
            volumes
                .iter()
                .all(|(_, value)| value.is_finite() && (0.0..=1.0).contains(value)),
            "CoreAudio returned an invalid hardware volume"
        );
        let mut mutes = Vec::new();
        if let Ok(muted) = object.get_property(DEVICE_OUTPUT_MUTE.for_element(0)) {
            mutes.push((0, muted));
        } else if volumes[0].0 != 0 {
            for &(channel, _) in &volumes {
                if let Ok(muted) = object.get_property(DEVICE_OUTPUT_MUTE.for_element(channel)) {
                    mutes.push((channel, muted));
                }
            }
        }
        let complete_mute = mutes.first().is_some_and(|(channel, _)| *channel == 0)
            || (!mutes.is_empty() && mutes.len() == volumes.len());
        let writable = complete_mute
            && volumes.iter().all(|(channel, _)| {
                object
                    .is_settable(DEVICE_OUTPUT_VOLUME_SCALAR.for_element(*channel))
                    .unwrap_or(false)
            })
            && mutes.iter().all(|(channel, _)| {
                object
                    .is_settable(DEVICE_OUTPUT_MUTE.for_element(*channel))
                    .unwrap_or(false)
            });
        let controls = Self {
            volumes,
            mutes,
            writable,
        };
        controls.remember_balance(&endpoint.uid)?;
        Ok(controls)
    }

    fn state(&self) -> VolumeState {
        VolumeState {
            level: self
                .volumes
                .iter()
                .map(|(_, level)| *level)
                .fold(0.0, f32::max),
            muted: !self.mutes.is_empty() && self.mutes.iter().all(|(_, muted)| *muted),
        }
    }

    fn remember_balance(&self, uid: &str) -> Result<()> {
        let peak = self.state().level;
        if self.volumes[0].0 == 0 || peak <= 0.0 {
            return Ok(());
        }
        let mut balances = BALANCES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("CoreAudio channel balance state is unavailable"))?;
        if balances.len() >= MAX_BALANCES && !balances.contains_key(uid) {
            balances.pop_first();
        }
        balances.insert(
            uid.to_owned(),
            self.volumes
                .iter()
                .map(|(channel, level)| (*channel, *level / peak))
                .collect(),
        );
        Ok(())
    }

    fn write(
        &self,
        endpoint: &Endpoint,
        state: VolumeState,
        cancel: &CancellationToken,
    ) -> Result<()> {
        ensure!(
            state.level.is_finite() && (0.0..=1.0).contains(&state.level),
            "Output volume must be between zero and one"
        );
        let object = &endpoint.object;
        // Validate every control before the first write. A missing mute control
        // still allows a manual volume-only change if mute remains unchanged.
        ensure!(
            self.volumes.iter().all(|(channel, _)| object
                .is_settable(DEVICE_OUTPUT_VOLUME_SCALAR.for_element(*channel))
                .unwrap_or(false)),
            "This output's hardware volume is read-only"
        );
        let mute_changed = state.muted != self.state().muted;
        ensure!(
            !mute_changed || self.writable,
            "This output has no writable hardware mute control"
        );
        if mute_changed && state.muted {
            for &(channel, _) in &self.mutes {
                ensure!(
                    !cancel.is_cancelled(),
                    "The selected output changed before its volume could be updated"
                );
                object.set_property(DEVICE_OUTPUT_MUTE.for_element(channel), true)?;
            }
        }
        let balance = BALANCES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("CoreAudio channel balance state is unavailable"))?
            .get(&endpoint.uid)
            .cloned()
            .unwrap_or_default();
        let peak = self.state().level;
        for &(channel, old_level) in &self.volumes {
            let ratio = if channel == 0 {
                1.0
            } else if peak > 0.0 {
                old_level / peak
            } else {
                balance
                    .iter()
                    .find(|(saved_channel, _)| *saved_channel == channel)
                    .map_or(1.0, |(_, ratio)| *ratio)
            };
            ensure!(
                !cancel.is_cancelled(),
                "The selected output changed before its volume could be updated"
            );
            object
                .set_property(
                    DEVICE_OUTPUT_VOLUME_SCALAR.for_element(channel),
                    (state.level * ratio).clamp(0.0, 1.0),
                )
                .context("setting CoreAudio hardware volume")?;
        }
        if mute_changed && !state.muted {
            for &(channel, _) in &self.mutes {
                ensure!(
                    !cancel.is_cancelled(),
                    "The selected output changed before its volume could be updated"
                );
                object.set_property(DEVICE_OUTPUT_MUTE.for_element(channel), false)?;
            }
        }
        Ok(())
    }
}

pub(super) async fn inspect(capture: &str, playback: &str) -> Result<Snapshot> {
    let capture = capture.to_owned();
    let playback = playback.to_owned();
    tokio::task::spawn_blocking(move || {
        let physical = resolve(&playback, DeviceDirection::Output)?;
        ensure!(!matches!(physical.uid.as_str(), BABEL_SPEAKER_UID | BABEL_MICROPHONE_UID), "A physical output is required for volume control");
        let physical_controls = Controls::read(&physical)?;
        let physical_state = physical_controls.state();
        let controls = resolve(&capture, DeviceDirection::Input).ok()
            .filter(|endpoint| endpoint.uid == BABEL_SPEAKER_UID)
            .and_then(|endpoint| Controls::read(&endpoint).ok());
        let synchronized = physical_controls.writable && controls.as_ref().is_some_and(|controls| controls.writable);
        let limitation = if !physical_controls.writable {
            Some("The physical output does not expose writable volume and mute controls to macOS. Use its hardware controls.".into())
        } else if controls.as_ref().is_none_or(|controls| !controls.writable) {
            Some("System volume synchronization requires the updated Babel Audio driver. Other virtual devices do not expose Babel's control-only output controls.".into())
        } else { None };
        Ok(Snapshot { virtual_state: controls.as_ref().map_or(physical_state, Controls::state), physical_state, synchronized, limitation })
    }).await.context("CoreAudio volume inspection worker failed")?
}

pub(super) async fn set_virtual(
    capture: &str,
    state: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let capture = capture.to_owned();
    tokio::task::spawn_blocking(move || {
        let endpoint = resolve(&capture, DeviceDirection::Input)?;
        ensure!(
            endpoint.uid == BABEL_SPEAKER_UID,
            "System volume synchronization requires Babel Speaker"
        );
        Controls::read(&endpoint)?.write(&endpoint, state, &cancel)
    })
    .await
    .context("CoreAudio virtual volume worker failed")?
}

pub(super) async fn set_physical(
    playback: &str,
    state: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let playback = playback.to_owned();
    tokio::task::spawn_blocking(move || {
        let endpoint = resolve(&playback, DeviceDirection::Output)?;
        ensure!(
            !matches!(
                endpoint.uid.as_str(),
                BABEL_SPEAKER_UID | BABEL_MICROPHONE_UID
            ),
            "A physical output is required for volume control"
        );
        Controls::read(&endpoint)?.write(&endpoint, state, &cancel)
    })
    .await
    .context("CoreAudio physical volume worker failed")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_mapping_uses_actual_stream_ranges_without_partial_updates() {
        let mut channels = BTreeSet::new();
        add_channel_range(&mut channels, 1, 2).unwrap();
        add_channel_range(&mut channels, 5, 2).unwrap();
        assert_eq!(channels.iter().copied().collect::<Vec<_>>(), [1, 2, 5, 6]);
        let original = channels.clone();
        for (start, count) in [(0, 2), (3, 0), (2, 4), (1, 64), (32, 2), (u32::MAX, 2)] {
            assert!(add_channel_range(&mut channels, start, count).is_err());
            assert_eq!(
                channels, original,
                "invalid stream ranges must not be partially accepted"
            );
        }
        add_channel_range(&mut channels, 32, 1).unwrap();
        assert!(channels.contains(&32));
    }
}
