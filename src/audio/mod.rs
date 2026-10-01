//! Bounded audio transport. No cloud or configuration work runs on audio callbacks.

use std::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64},
    time::Instant,
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub(crate) mod activity;
#[cfg(target_os = "linux")]
mod linux;
pub(crate) mod mirror;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod native_ids;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod native_lease;
pub mod passthrough;
pub(crate) mod resample;
pub mod speech;
pub mod switching;

#[cfg(target_os = "linux")]
pub use linux::{
    capture, devices, install_virtual_devices, original_format, playback, uninstall_virtual_devices,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::{
    capture, devices, install_virtual_devices, original_format, playback, uninstall_virtual_devices,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub direction: DeviceDirection,
    pub is_virtual: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioOptions {
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_ms: u32,
    pub latency_ms: u32,
    pub queue_ms: u32,
}

impl AudioOptions {
    fn validate(self) -> Result<Self> {
        ensure!(
            (8_000..=192_000).contains(&self.sample_rate),
            "audio sample rate must be 8000..192000 Hz"
        );
        ensure!(
            (1..=32).contains(&self.channels),
            "audio channels must be 1..32"
        );
        ensure!(
            (5..=200).contains(&self.frame_ms),
            "audio frame size must be 5..200 ms"
        );
        ensure!(
            (5..=500).contains(&self.latency_ms),
            "audio latency must be 5..500 ms"
        );
        ensure!(
            (self.frame_ms..=5_000).contains(&self.queue_ms),
            "audio queue must be at least one frame and at most 5000 ms"
        );
        ensure!(
            (u64::from(self.sample_rate) * u64::from(self.frame_ms)) % 1000 == 0,
            "frame size must contain an integer number of samples"
        );
        Ok(self)
    }

    fn frame_samples(self) -> usize {
        (u64::from(self.sample_rate) * u64::from(self.frame_ms) * u64::from(self.channels) / 1000)
            as usize
    }
}

#[derive(Debug)]
pub struct PcmFrame {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub captured_at: Instant,
}

/// Full-resolution interleaved original PCM. Speech conversion belongs to a sidecar.
#[derive(Debug, Clone)]
pub struct OriginalFrame {
    pub samples: std::sync::Arc<[f32]>,
    pub sample_rate: u32,
    pub channels: u16,
    pub captured_at: Instant,
}

#[derive(Debug)]
pub enum PlaybackCommand {
    Original {
        samples: std::sync::Arc<[f32]>,
        generation: u64,
    },
    Audio {
        samples: Vec<i16>,
        generation: u64,
    },
    /// Discard audio already queued to the audio backend (for interrupted turns).
    Flush,
}

#[derive(Debug, Default)]
pub struct AudioStats {
    /// Shared-mode translated output has separate ownership from original routing.
    pub translated_playback: bool,
    /// Used only by the capture control worker, never by native audio callbacks.
    pub command_tap: Option<std::sync::Weak<crate::commands::CommandService>>,
    /// Shares speaker originals with a playback-only microphone route. Never
    /// populated for physical microphone capture or used by device callbacks.
    pub(crate) original_mirror: Option<std::sync::Arc<mirror::Source>>,
    /// Optional post-processing speaker playback source. Kept separate from
    /// originals so translated audio cannot enter history or recognition.
    pub(crate) playback_mirror: Option<std::sync::Arc<mirror::Source>>,
    pub captured_frames: AtomicU64,
    pub dropped_frames: AtomicU64,
    /// Captured blocks discarded before original sidecars could retain them.
    /// Native callbacks may contain multiple logical frames. Playback freshness
    /// drops and auxiliary command/mirror copies must never increment this.
    pub capture_lost_frames: AtomicU64,
    pub processing_dropped_frames: AtomicU64,
    pub sidecar_dropped_frames: AtomicU64,
    pub underruns: AtomicU64,
    pub realtime_denied: AtomicBool,
    /// Incrementing this epoch interrupts playback even when its channel is full.
    pub playback_generation: AtomicU64,
    /// Normalized RMS f32 bits, updated by the passthrough worker, not callbacks.
    pub passthrough_level: AtomicU32,
    /// Set only by control workers; callbacks never lock these fields.
    pub capture_error: std::sync::Mutex<Option<String>>,
    pub playback_error: std::sync::Mutex<Option<String>>,
}

impl AudioStats {
    /// Atomic-only accounting is safe on the native callback. Retention failure
    /// reporting belongs to the separate original-processing consumer.
    fn record_capture_loss(&self) {
        self.capture_lost_frames
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.dropped_frames
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_audio_options_are_rejected_before_allocating() {
        let valid = AudioOptions {
            sample_rate: 16_000,
            channels: 1,
            frame_ms: 20,
            latency_ms: 40,
            queue_ms: 200,
        };
        assert_eq!(valid.validate().unwrap().frame_samples(), 320);
        assert!(
            AudioOptions {
                sample_rate: u32::MAX,
                ..valid
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioOptions {
                frame_ms: 0,
                ..valid
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioOptions {
                queue_ms: 1,
                ..valid
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioOptions {
                sample_rate: 44_100,
                frame_ms: 5,
                ..valid
            }
            .validate()
            .is_err()
        );
    }
}
