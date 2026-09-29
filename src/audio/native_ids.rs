//! Persist OS endpoint identities, never positions in an enumeration.
use super::DeviceDirection;
use anyhow::{Result, bail, ensure};

// Part of the native driver protocol. Keep these in sync with
// native/macos; the displayed device names can be changed.
pub(super) const BABEL_MICROPHONE_UID: &str = "org.babel.audio.microphone.v1";
pub(super) const BABEL_SPEAKER_UID: &str = "org.babel.audio.speaker.v1";
pub(super) const BABEL_WINDOWS_ADAPTER: &str = "Babel Audio v1";

pub(super) fn babel_coreaudio_device(native_id: &str) -> bool {
    matches!(
        native_id.strip_prefix("coreaudio:"),
        Some(BABEL_MICROPHONE_UID | BABEL_SPEAKER_UID)
    )
}

/// Match driver-defined metadata, not PKEY_Device_FriendlyName. WASAPI
/// endpoint IDs remain opaque and are never reconstructed from these strings.
#[cfg(any(target_os = "windows", test))]
pub(super) fn babel_windows_cable(
    description: &str,
    adapter: &str,
    direction: DeviceDirection,
) -> Option<&'static str> {
    if adapter != BABEL_WINDOWS_ADAPTER {
        return None;
    }
    match (direction, description) {
        (DeviceDirection::Output, "Babel Microphone Feed")
        | (DeviceDirection::Input, "Babel Microphone") => Some(BABEL_MICROPHONE_UID),
        (DeviceDirection::Output, "Babel Speaker")
        | (DeviceDirection::Input, "Babel Speaker Monitor") => Some(BABEL_SPEAKER_UID),
        _ => None,
    }
}

pub(super) fn babel_virtual_device(native_id: &str, adapter: Option<&str>) -> bool {
    // CPAL exposes the interface friendly name in DeviceDescription.driver().
    // This flag only classifies the list; the Windows activity monitor also
    // requires the exact endpoint role and a unique opposite-side endpoint.
    babel_coreaudio_device(native_id)
        || (native_id.starts_with("wasapi:") && adapter == Some(BABEL_WINDOWS_ADAPTER))
}

pub(super) enum Selection<'a> {
    Stable(&'a str),
    LegacyName(&'a str),
}

fn prefix(direction: DeviceDirection) -> &'static str {
    match direction {
        DeviceDirection::Input => "input:",
        DeviceDirection::Output => "output:",
    }
}

pub(super) fn persistent_id(direction: DeviceDirection, native_id: &str) -> String {
    format!("{}{native_id}", prefix(direction))
}

pub(super) fn parse(id: &str, direction: DeviceDirection) -> Result<Selection<'_>> {
    let rest = id.strip_prefix(prefix(direction)).ok_or_else(|| {
        anyhow::anyhow!(
            "select an explicit native device ID for the correct input/output direction"
        )
    })?;
    let (kind, value) = rest
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid native device ID"))?;
    ensure!(!value.is_empty(), "empty native device identity");
    if kind.parse::<usize>().is_ok() {
        // Old settings stored input:<enumeration index>:<display name>. Ignore
        // that obsolete index: it can identify unrelated hardware after hotplug.
        Ok(Selection::LegacyName(value))
    } else if matches!(kind, "coreaudio" | "wasapi") {
        Ok(Selection::Stable(rest))
    } else {
        bail!("unsupported native audio backend in device ID")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_identity_keeps_uid_punctuation_and_direction() {
        let raw = "coreaudio:BlackHole:2ch:UID";
        let saved = persistent_id(DeviceDirection::Output, raw);
        assert!(
            matches!(parse(&saved, DeviceDirection::Output).unwrap(), Selection::Stable(id) if id == raw)
        );
        assert!(parse(&saved, DeviceDirection::Input).is_err());
        let raw = "wasapi:{0.0.1.00000000}.{01234567-89ab-cdef-0000-0123456789ab}";
        assert!(
            matches!(parse(&persistent_id(DeviceDirection::Input, raw), DeviceDirection::Input).unwrap(), Selection::Stable(id) if id == raw)
        );
    }

    #[test]
    fn legacy_index_is_never_treated_as_a_hardware_identity() {
        assert!(
            matches!(parse("input:19:USB:Microphone", DeviceDirection::Input).unwrap(), Selection::LegacyName(name) if name == "USB:Microphone")
        );
        for id in [
            "default",
            "input:",
            "input:wasapi:",
            "input:unknown:device",
            "output:1:mic",
        ] {
            assert!(parse(id, DeviceDirection::Input).is_err(), "{id}");
        }
    }

    #[test]
    fn babel_duplex_devices_have_two_exact_stable_identities() {
        for uid in [BABEL_MICROPHONE_UID, BABEL_SPEAKER_UID] {
            let native = format!("coreaudio:{uid}");
            assert!(babel_coreaudio_device(&native));
            for direction in [DeviceDirection::Input, DeviceDirection::Output] {
                let saved = persistent_id(direction, &native);
                assert!(
                    matches!(parse(&saved, direction).unwrap(), Selection::Stable(id) if id == native)
                );
            }
        }
        assert_ne!(BABEL_MICROPHONE_UID, BABEL_SPEAKER_UID);
        for unrelated in [
            "Babel Microphone",
            "coreaudio:Babel Microphone",
            "coreaudio:org.babel.audio.microphone.v1.extra",
            "coreaudio:org.babel.audio.speaker.v2",
            "wasapi:org.babel.audio.microphone.v1",
        ] {
            assert!(!babel_coreaudio_device(unrelated), "{unrelated}");
        }
    }

    #[test]
    fn babel_driver_classification_does_not_use_display_names_or_substrings() {
        assert!(babel_virtual_device(
            "coreaudio:org.babel.audio.microphone.v1",
            None
        ));
        assert!(babel_virtual_device(
            "wasapi:opaque-endpoint-id",
            Some(BABEL_WINDOWS_ADAPTER)
        ));
        for (id, adapter) in [
            ("wasapi:Babel Microphone", None),
            ("wasapi:opaque", Some("Babel Audio v1 extra")),
            ("wasapi:opaque", Some("Babel Microphone")),
            ("coreaudio:physical", Some(BABEL_WINDOWS_ADAPTER)),
        ] {
            assert!(!babel_virtual_device(id, adapter));
        }
    }

    #[test]
    fn windows_babel_driver_metadata_enforces_all_four_endpoint_roles() {
        for (direction, description, expected) in [
            (
                DeviceDirection::Output,
                "Babel Microphone Feed",
                BABEL_MICROPHONE_UID,
            ),
            (
                DeviceDirection::Input,
                "Babel Microphone",
                BABEL_MICROPHONE_UID,
            ),
            (DeviceDirection::Output, "Babel Speaker", BABEL_SPEAKER_UID),
            (
                DeviceDirection::Input,
                "Babel Speaker Monitor",
                BABEL_SPEAKER_UID,
            ),
        ] {
            assert_eq!(
                babel_windows_cable(description, BABEL_WINDOWS_ADAPTER, direction),
                Some(expected)
            );
            let wrong_direction = match direction {
                DeviceDirection::Input => DeviceDirection::Output,
                DeviceDirection::Output => DeviceDirection::Input,
            };
            assert_eq!(
                babel_windows_cable(description, BABEL_WINDOWS_ADAPTER, wrong_direction),
                None
            );
            assert_eq!(
                babel_windows_cable(description, "unrelated driver", direction),
                None
            );
        }
    }
}
