//! Persist OS endpoint identities, never positions in an enumeration.
use super::DeviceDirection;
use anyhow::{Result, bail, ensure};

/// USB product names published by the Virtual Cables loopback utility.
#[cfg(any(target_os = "windows", test))]
pub(super) fn usb_virtual_cable_number(product: &str) -> Option<u8> {
    let number = product.trim().strip_prefix("Virtual Cable ")?;
    if number.len() != 2 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number
        .parse()
        .ok()
        .filter(|number| (1..=32).contains(number))
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
}
