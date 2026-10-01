//! Safe, synchronous endpoint-volume boundary. Call only on a control thread.
//! COM interfaces never escape the initialized apartment or enter audio callbacks.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod win32;
#[cfg(windows)]
pub use win32::{inspect, set_physical, set_virtual};

#[cfg(any(windows, test))]
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub virtual_state: VolumeState,
    pub physical_state: VolumeState,
    pub synchronized: bool,
    pub limitation: Option<String>,
}

#[cfg(any(windows, test))]
fn validate(state: VolumeState) -> Result<()> {
    ensure!(
        state.level.is_finite() && (0.0..=1.0).contains(&state.level),
        "Windows endpoint volume must be between zero and one"
    );
    Ok(())
}

#[cfg(any(windows, test))]
const DRIVER: &str = "org.babel.audio.driver.v1";
#[cfg(any(windows, test))]
const CAPABILITY: &str = "control-only-volume-v1";
#[cfg(any(windows, test))]
const SPEAKER_CAPTURE: &str = "babel:speaker:capture:v1";
#[cfg(any(windows, test))]
const SPEAKER_RENDER: &str = "babel:speaker:render:v1";

#[cfg(any(windows, test))]
fn compatible(driver: &str, role: &str, capability: &str, expected_role: &str) -> bool {
    driver == DRIVER && role == expected_role && capability == CAPABILITY
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Write {
    Level(f32),
    Mute(bool),
}

#[cfg(any(windows, test))]
fn apply(
    state: VolumeState,
    cancelled: &impl Fn() -> bool,
    mut write: impl FnMut(Write) -> Result<()>,
) -> Result<()> {
    validate(state)?;
    let writes = if state.muted {
        [Write::Mute(true), Write::Level(state.level)]
    } else {
        [Write::Level(state.level), Write::Mute(false)]
    };
    for next in writes {
        ensure!(!cancelled(), "Windows volume update was cancelled");
        write(next)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_control_only_speaker_roles_authorize_synchronization() {
        assert!(compatible(
            DRIVER,
            SPEAKER_CAPTURE,
            CAPABILITY,
            SPEAKER_CAPTURE
        ));
        assert!(compatible(
            DRIVER,
            SPEAKER_RENDER,
            CAPABILITY,
            SPEAKER_RENDER
        ));
        for (driver, role, capability) in [
            (DRIVER, SPEAKER_RENDER, ""),
            (DRIVER, SPEAKER_RENDER, "control-only-volume-v2"),
            ("VB-CABLE", SPEAKER_RENDER, CAPABILITY),
            (DRIVER, "babel:mic:render:v1", CAPABILITY),
        ] {
            assert!(!compatible(driver, role, capability, SPEAKER_RENDER));
        }
    }

    #[test]
    fn invalid_levels_are_rejected_before_any_os_write() {
        for level in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            assert!(
                validate(VolumeState {
                    level,
                    muted: false
                })
                .is_err()
            );
        }
        for level in [0.0, 0.5, 1.0] {
            assert!(validate(VolumeState { level, muted: true }).is_ok());
        }
    }

    #[test]
    fn cancellation_rejects_queued_writes_and_each_remaining_write() {
        let calls = std::cell::RefCell::new(Vec::new());
        let state = VolumeState {
            level: 0.8,
            muted: true,
        };
        assert!(
            apply(state, &|| true, |next| {
                calls.borrow_mut().push(next);
                Ok(())
            })
            .is_err()
        );
        assert!(calls.borrow().is_empty());
        assert!(
            apply(state, &|| !calls.borrow().is_empty(), |next| {
                calls.borrow_mut().push(next);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(*calls.borrow(), [Write::Mute(true)]);
    }

    #[test]
    fn unmute_occurs_only_after_a_successful_master_level_write() {
        let mut calls = Vec::new();
        let state = VolumeState {
            level: 0.4,
            muted: false,
        };
        assert!(
            apply(state, &|| false, |next| {
                calls.push(next);
                anyhow::bail!("device disappeared")
            })
            .is_err()
        );
        assert_eq!(calls, [Write::Level(0.4)]);
    }
}
