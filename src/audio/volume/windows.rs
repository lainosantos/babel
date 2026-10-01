//! Windows master-volume operations stay off the routing and callback threads.
use anyhow::{Context, Result};
use cpal::traits::DeviceTrait;
use tokio_util::sync::CancellationToken;

use super::{Snapshot, VolumeState};
use crate::audio::{DeviceDirection, native};

fn identity(selection: &str, direction: DeviceDirection) -> Result<String> {
    Ok(native::resolve(selection, direction)?
        .id()
        .context("reading Windows endpoint identity for volume control")?
        .id()
        .to_owned())
}

pub(super) async fn inspect(capture: &str, playback: &str) -> Result<Snapshot> {
    let (capture, playback) = (capture.to_owned(), playback.to_owned());
    tokio::task::spawn_blocking(move || {
        let capture = identity(&capture, DeviceDirection::Input).unwrap_or_default();
        let playback = identity(&playback, DeviceDirection::Output)?;
        let snapshot = babel_windows_volume::inspect(&capture, &playback)?;
        Ok(Snapshot {
            virtual_state: VolumeState {
                level: snapshot.virtual_state.level,
                muted: snapshot.virtual_state.muted,
            },
            physical_state: VolumeState {
                level: snapshot.physical_state.level,
                muted: snapshot.physical_state.muted,
            },
            synchronized: snapshot.synchronized,
            limitation: snapshot.limitation,
        })
    })
    .await
    .context("Windows volume inspection worker failed")?
}

pub(super) async fn set_virtual(
    capture: &str,
    state: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let capture = capture.to_owned();
    tokio::task::spawn_blocking(move || {
        let capture = identity(&capture, DeviceDirection::Input)?;
        babel_windows_volume::set_virtual(
            &capture,
            babel_windows_volume::VolumeState {
                level: state.level,
                muted: state.muted,
            },
            || cancel.is_cancelled(),
        )
    })
    .await
    .context("Windows virtual-volume worker failed")?
}

pub(super) async fn set_physical(
    playback: &str,
    state: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let playback = playback.to_owned();
    tokio::task::spawn_blocking(move || {
        let playback = identity(&playback, DeviceDirection::Output)?;
        babel_windows_volume::set_physical(
            &playback,
            babel_windows_volume::VolumeState {
                level: state.level,
                muted: state.muted,
            },
            || cancel.is_cancelled(),
        )
    })
    .await
    .context("Windows physical-volume worker failed")?
}
