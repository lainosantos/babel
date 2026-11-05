//! Windows master-volume operations stay off the routing and callback threads.
use anyhow::{Context, Result};
use cpal::traits::DeviceTrait;
use tokio_util::sync::CancellationToken;

use super::{Snapshot, VolumeState};

use crate::audio::native;

fn identity(selection: &str) -> Result<String> {
    Ok(native::resolve_physical_output(selection)?
        .id()
        .context("reading Windows endpoint identity for volume control")?
        .id()
        .to_owned())
}

pub(super) async fn inspect(_capture: &str, playback: &str) -> Result<Snapshot> {
    let playback = playback.to_owned();
    tokio::task::spawn_blocking(move || {
        let state = babel_windows_volume::inspect(&identity(&playback)?)?;
        Ok(Snapshot::physical(
            VolumeState {
                level: state.level,
                muted: state.muted,
            },
            None,
        ))
    })
    .await
    .context("Windows volume inspection worker failed")?
}

pub(super) async fn set_physical(
    playback: &str,
    state: VolumeState,
    cancel: CancellationToken,
) -> Result<()> {
    let playback = playback.to_owned();
    tokio::task::spawn_blocking(move || {
        let playback = identity(&playback)?;
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
