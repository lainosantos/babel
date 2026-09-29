//! HAL property values; all CoreAudio layouts/constant lookups live in the SDK shim.
use crate::{MICROPHONE, SPEAKER};
use babel_hal_core::{LATENCY_FRAMES, SAMPLE_RATE as RATE, TIMESTAMP_PERIOD};
pub mod property {
    include!(concat!(env!("OUT_DIR"), "/properties.rs"));
}
use property::*;
pub const PLUGIN: u32 = 1;
pub const BOX: u32 = 2;
pub const MIC: u32 = 10;
pub const SPEAKER_DEVICE: u32 = 20;
pub const INPUT_SCOPE: u32 = 1;
pub const OUTPUT_SCOPE: u32 = 2;
pub const GLOBAL_SCOPE: u32 = 0;
pub const BOX_UID_VALUE: &str = "org.babel.audio.box.v1";

pub enum Value {
    U32(u32),
    List(&'static [u32]),
    Number(f64),
    Text(&'static str),
    RateRange,
    Format,
    RangedFormat,
    StereoLayout,
    Transport,
    Terminal,
}
pub fn valid_object(object: u32) -> bool {
    matches!(
        object,
        PLUGIN | BOX | MIC | 11 | 12 | SPEAKER_DEVICE | 21 | 22
    )
}
pub fn device(object: u32) -> Option<&'static babel_hal_core::Cable> {
    match object {
        MIC => Some(&MICROPHONE),
        SPEAKER_DEVICE => Some(&SPEAKER),
        _ => None,
    }
}
pub fn stream(object: u32) -> Option<(u32, bool)> {
    match object {
        11 => Some((MIC, true)),
        12 => Some((MIC, false)),
        21 => Some((SPEAKER_DEVICE, true)),
        22 => Some((SPEAKER_DEVICE, false)),
        _ => None,
    }
}
pub fn class(object: u32) -> u32 {
    u32::from_be_bytes(match object {
        PLUGIN => *b"aplg",
        BOX => *b"abox",
        MIC | SPEAKER_DEVICE => *b"adev",
        _ => *b"astr",
    })
}
pub fn settable(object: u32, selector: u32) -> bool {
    (device(object).is_some() && selector == SAMPLE_RATE)
        || (stream(object).is_some()
            && matches!(selector, STREAM_ACTIVE | VIRTUAL_FORMAT | PHYSICAL_FORMAT))
        || (object == BOX && selector == ACQUIRED)
}
pub fn value(
    object: u32,
    selector: u32,
    scope: u32,
    element: u32,
    qualifier: &str,
) -> Option<Value> {
    if !valid_object(object) || scope > OUTPUT_SCOPE {
        return None;
    }
    if selector == ELEMENT_NAME && device(object).is_some() && element <= 2 {
        return Some(Value::Text(match element {
            0 => "Master",
            1 => "Left",
            _ => "Right",
        }));
    }
    if element != 0 {
        return None;
    }
    match selector {
        BASE_CLASS => return Some(Value::U32(u32::from_be_bytes(*b"aobj"))),
        CLASS => return Some(Value::U32(class(object))),
        OWNER => {
            return Some(Value::U32(if object == PLUGIN {
                0
            } else {
                stream(object).map_or(PLUGIN, |(device, _)| device)
            }));
        }
        MANUFACTURER => return Some(Value::Text("Babel")),
        NAME => {
            return Some(Value::Text(match object {
                PLUGIN => "Babel Audio",
                BOX => "Babel Virtual Audio",
                MIC => "Babel Microphone",
                SPEAKER_DEVICE => "Babel Speaker",
                11 | 21 => "Input",
                _ => "Output",
            }));
        }
        CONTROLS => return Some(Value::List(&[])),
        OWNED_OBJECTS => {
            return Some(Value::List(match (object, scope) {
                (PLUGIN, _) => &[BOX, MIC, SPEAKER_DEVICE],
                (MIC, INPUT_SCOPE) => &[11],
                (MIC, OUTPUT_SCOPE) => &[12],
                (MIC, _) => &[11, 12],
                (SPEAKER_DEVICE, INPUT_SCOPE) => &[21],
                (SPEAKER_DEVICE, OUTPUT_SCOPE) => &[22],
                (SPEAKER_DEVICE, _) => &[21, 22],
                _ => &[],
            }));
        }
        _ => {}
    }
    if object == PLUGIN {
        return Some(match selector {
            BOX_LIST => Value::List(&[BOX]),
            DEVICE_LIST => Value::List(&[MIC, SPEAKER_DEVICE]),
            UID_TO_BOX => Value::U32(if qualifier == BOX_UID_VALUE { BOX } else { 0 }),
            UID_TO_DEVICE => Value::U32(match qualifier {
                babel_hal_core::MICROPHONE_UID => MIC,
                babel_hal_core::SPEAKER_UID => SPEAKER_DEVICE,
                _ => 0,
            }),
            RESOURCE_BUNDLE => Value::Text(""),
            _ => return None,
        });
    }
    if object == BOX {
        return Some(match selector {
            BOX_UID => Value::Text(BOX_UID_VALUE),
            BOX_TRANSPORT | DEVICE_TRANSPORT => Value::Transport,
            HAS_AUDIO | ACQUIRED => Value::U32(1),
            HAS_VIDEO | HAS_MIDI | PROTECTED | ACQUISITION_FAILED => Value::U32(0),
            BOX_DEVICES => Value::List(&[MIC, SPEAKER_DEVICE]),
            _ => return None,
        });
    }
    if let Some(cable) = device(object) {
        return Some(match selector {
            DEVICE_UID => Value::Text(if object == MIC {
                babel_hal_core::MICROPHONE_UID
            } else {
                babel_hal_core::SPEAKER_UID
            }),
            MODEL_UID => Value::Text("org.babel.audio.duplex48.v1"),
            DEVICE_TRANSPORT | BOX_TRANSPORT => Value::Transport,
            RELATED_DEVICES => Value::List(if object == MIC {
                &[MIC]
            } else {
                &[SPEAKER_DEVICE]
            }),
            CLOCK_DOMAIN | HIDDEN => Value::U32(0),
            ALIVE => Value::U32(1),
            RUNNING => Value::U32(u32::from(cable.is_running())),
            SAMPLE_RATE => Value::Number(f64::from(RATE)),
            SAMPLE_RATES => Value::RateRange,
            ZERO_PERIOD => Value::U32(TIMESTAMP_PERIOD as u32),
            STREAMS => Value::List(match (object, scope) {
                (MIC, INPUT_SCOPE) => &[11],
                (MIC, OUTPUT_SCOPE) => &[12],
                (MIC, _) => &[11, 12],
                (_, INPUT_SCOPE) => &[21],
                (_, OUTPUT_SCOPE) => &[22],
                _ => &[21, 22],
            }),
            DEFAULT_DEVICE | DEFAULT_SYSTEM if scope != GLOBAL_SCOPE => Value::U32(u32::from(
                (object == MIC && scope == INPUT_SCOPE && selector != DEFAULT_SYSTEM)
                    || (object == SPEAKER_DEVICE && scope == OUTPUT_SCOPE),
            )),
            LATENCY if scope != GLOBAL_SCOPE => Value::U32(if scope == INPUT_SCOPE {
                LATENCY_FRAMES as u32
            } else {
                0
            }),
            SAFETY_OFFSET if scope != GLOBAL_SCOPE => Value::U32(0),
            STEREO_CHANNELS if scope != GLOBAL_SCOPE => Value::List(&[1, 2]),
            CHANNEL_LAYOUT if scope != GLOBAL_SCOPE => Value::StereoLayout,
            _ => return None,
        });
    }
    if let Some((owner, input)) = stream(object) {
        return Some(match selector {
            STREAM_ACTIVE => Value::U32(u32::from(device(owner)?.active(input))),
            STREAM_DIRECTION => Value::U32(u32::from(input)),
            TERMINAL_TYPE => Value::Terminal,
            START_CHANNEL => Value::U32(1),
            STREAM_LATENCY => Value::U32(0),
            VIRTUAL_FORMAT | PHYSICAL_FORMAT => Value::Format,
            VIRTUAL_FORMATS | PHYSICAL_FORMATS => Value::RangedFormat,
            _ => return None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_uids_enumerate_two_independent_duplex_devices() {
        assert!(matches!(
            value(PLUGIN, DEVICE_LIST, 0, 0, ""),
            Some(Value::List(&[10, 20]))
        ));
        for (uid, id, input, output) in [
            (babel_hal_core::MICROPHONE_UID, 10, 11, 12),
            (babel_hal_core::SPEAKER_UID, 20, 21, 22),
        ] {
            assert!(
                matches!(value(PLUGIN,UID_TO_DEVICE,0,0,uid),Some(Value::U32(actual)) if actual==id)
            );
            assert!(
                matches!(value(id,STREAMS,INPUT_SCOPE,0,""),Some(Value::List(ids)) if ids==[input])
            );
            assert!(
                matches!(value(id,STREAMS,OUTPUT_SCOPE,0,""),Some(Value::List(ids)) if ids==[output])
            );
            assert_eq!(stream(input), Some((id, true)));
            assert_eq!(stream(output), Some((id, false)));
        }
        assert!(matches!(
            value(BOX, ACQUIRED, 0, 0, ""),
            Some(Value::U32(1))
        ));
    }
    #[test]
    fn static_format_unknown_selectors_and_wrong_elements_are_honest() {
        assert!(matches!(
            value(10, SAMPLE_RATE, 0, 0, ""),
            Some(Value::Number(48000.0))
        ));
        assert!(value(10, LATENCY, 0, 0, "").is_none());
        assert!(matches!(
            value(10, LATENCY, INPUT_SCOPE, 0, ""),
            Some(Value::U32(4096))
        ));
        assert!(value(999, CLASS, 0, 0, "").is_none());
        assert!(value(10, 9999, 0, 0, "").is_none());
        assert!(value(10, CLASS, 0, 1, "").is_none());
        assert!(!settable(10, DEVICE_UID));
    }
}
