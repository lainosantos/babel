//! The only Rust crate allowed to cross the CoreAudio ABI boundary.
//! All raw-pointer preconditions are supplied by Apple's HAL or our SDK-typed
//! shim. No CoreAudio vtable or AudioServerPlugInIOCycleInfo layout is copied.
#![deny(unsafe_op_in_unsafe_fn)]
use babel_hal_core::{CHANNELS, Cable, MAX_FRAMES, sample_time, zero_timestamp};
#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::{
    ptr, slice,
    sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst},
};
mod model;
static MICROPHONE: Cable = Cable::new();
static SPEAKER: Cable = Cable::new();
static REFERENCES: AtomicU32 = AtomicU32::new(1);
static ANCHOR: AtomicU64 = AtomicU64::new(0);
static NUMER: AtomicU32 = AtomicU32::new(0);
static DENOM: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
pub struct BabelProperty {
    kind: u32,
    count: u32,
    values: [u32; 8],
    number: f64,
    text: *const u8,
}
impl BabelProperty {
    fn from_value(value: model::Value) -> Self {
        use model::Value;
        let mut out = Self {
            kind: 0,
            count: 0,
            values: [0; 8],
            number: 0.0,
            text: ptr::null(),
        };
        match value {
            Value::U32(value) => {
                out.kind = 1;
                out.count = 1;
                out.values[0] = value;
            }
            Value::List(values) => {
                out.kind = 2;
                out.count = values.len() as u32;
                for (slot, value) in out.values.iter_mut().zip(values) {
                    *slot = *value;
                }
            }
            Value::Number(value) => {
                out.kind = 3;
                out.number = value;
            }
            Value::Scalar(value) => {
                out.kind = 11;
                out.number = f64::from(value);
            }
            Value::DecibelRange => out.kind = 12,
            Value::OutputScope => out.kind = 13,
            Value::ScalarConversion => out.kind = 14,
            Value::Text(text) => {
                out.kind = 4;
                out.count = text.len() as u32;
                out.text = text.as_ptr();
            }
            Value::RateRange => out.kind = 5,
            Value::Format => out.kind = 6,
            Value::RangedFormat => out.kind = 7,
            Value::StereoLayout => out.kind = 8,
            Value::Transport => out.kind = 9,
            Value::Terminal => out.kind = 10,
        }
        out
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn BabelHALCreate(allocator: *const c_void, requested_type: *const c_void) -> *mut c_void;
}
/// # Safety
/// Called by CFPlugIn with valid CFAllocator/CFUUID objects from CoreFoundation.
#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BabelCreate(
    allocator: *const c_void,
    requested_type: *const c_void,
) -> *mut c_void {
    // SAFETY: the C shim is compiled against the current SDK and validates UUID.
    unsafe { BabelHALCreate(allocator, requested_type) }
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelRetain() -> u32 {
    REFERENCES
        .fetch_update(SeqCst, SeqCst, |v| Some(v.saturating_add(1)))
        .unwrap_or(0)
        .saturating_add(1)
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelRelease() -> u32 {
    REFERENCES
        .fetch_update(SeqCst, SeqCst, |v| Some(v.saturating_sub(1)))
        .unwrap_or(0)
        .saturating_sub(1)
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelInitialize(anchor: u64, numer: u32, denom: u32) -> i32 {
    if numer == 0 || denom == 0 {
        return -3;
    }
    ANCHOR.store(anchor, SeqCst);
    NUMER.store(numer, SeqCst);
    DENOM.store(denom, SeqCst);
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelValidObject(object: u32) -> u8 {
    u8::from(model::valid_object(object))
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelHasProperty(object: u32, property: u32, scope: u32, element: u32) -> u8 {
    u8::from(model::value(object, property, scope, element, "").is_some())
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelIsSettable(object: u32, property: u32) -> u8 {
    u8::from(model::settable(object, property))
}
/// # Safety
/// result points to one writable BabelProperty; qualifier is either null/zero
/// or a valid UTF-8 byte slice of qualifier_size bytes supplied by the C shim.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BabelGetProperty(
    object: u32,
    property: u32,
    scope: u32,
    element: u32,
    qualifier: *const u8,
    qualifier_size: u32,
    result: *mut BabelProperty,
) -> i32 {
    if result.is_null() || qualifier_size > 512 {
        return -3;
    }
    if !model::valid_object(object) {
        return -1;
    }
    let text = if qualifier_size == 0 {
        ""
    } else {
        if qualifier.is_null() {
            return -3;
        }
        // SAFETY: bounded, valid memory is owned by the shim for this call.
        let bytes = unsafe { slice::from_raw_parts(qualifier, qualifier_size as usize) };
        match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return -3,
        }
    };
    let Some(value) = model::value(object, property, scope, element, text) else {
        return -2;
    };
    // SAFETY: the caller allocated this private repr(C) result structure.
    unsafe {
        ptr::write(result, BabelProperty::from_value(value));
    }
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelSetActive(stream: u32, active: u32) -> i32 {
    let Some((owner, input)) = model::stream(stream) else {
        return -1;
    };
    let Some(cable) = model::device(owner) else {
        return -1;
    };
    if active > 1 {
        return -3;
    }
    i32::from(cable.set_active(input, active != 0))
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelSetLevel(object: u32, property: u32, value: f32) -> i32 {
    model::set_level(object, property, value)
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelSetMute(object: u32, value: u32) -> i32 {
    model::set_mute(object, value)
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelConvertLevel(to_decibels: u32, value: f32) -> f32 {
    if to_decibels != 0 {
        model::scalar_to_decibels(value)
    } else {
        model::decibels_to_scalar(value)
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn BabelDeviceAction(device: u32, client: u32, action: u32) -> i32 {
    let Some(cable) = model::device(device) else {
        return -1;
    };
    let ok = match action {
        0 => cable.add_client(client),
        1 => cable.remove_client(client),
        2 => cable.start(client),
        3 => cable.stop(client),
        _ => false,
    };
    if ok { 0 } else { -3 }
}
/// # Safety
/// All output pointers refer to writable HAL timestamp fields for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BabelZeroTimestamp(
    device: u32,
    now: u64,
    sample: *mut f64,
    host: *mut u64,
    seed: *mut u64,
) -> i32 {
    let Some(cable) = model::device(device) else {
        return -1;
    };
    if sample.is_null() || host.is_null() || seed.is_null() {
        return -3;
    }
    let Some((frames, ticks)) = zero_timestamp(
        ANCHOR.load(SeqCst),
        now,
        NUMER.load(SeqCst),
        DENOM.load(SeqCst),
    ) else {
        return -3;
    };
    // SAFETY: outputs were checked for null and are supplied by HAL via the shim.
    unsafe {
        ptr::write(sample, frames as f64);
        ptr::write(host, ticks);
        ptr::write(seed, cable.generation());
    }
    0
}
/// # Safety
/// data refers to at least frames*2 interleaved f32 samples, writable for input
/// and readable for WriteMix. HAL guarantees its lifetime and exclusivity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn BabelProcess(
    device: u32,
    stream: u32,
    read_input: u32,
    time: f64,
    frames: u32,
    data: *mut f32,
) -> i32 {
    let Some(cable) = model::device(device) else {
        return -1;
    };
    if model::stream(stream) != Some((device, read_input != 0)) || read_input > 1 {
        return -1;
    }
    if frames as usize > MAX_FRAMES {
        return -3;
    }
    if frames == 0 {
        return 0;
    }
    if data.is_null() || (data as usize) % std::mem::align_of::<f32>() != 0 {
        return -3;
    }
    let len = frames as usize * CHANNELS;
    let Some(timestamp) = sample_time(time) else {
        if read_input != 0 {
            // SAFETY: the HAL buffer is valid and bounded as checked above.
            // Preroll can carry a negative input sample time; it is silence,
            // never a reason to expose the contents of an old host buffer.
            unsafe { slice::from_raw_parts_mut(data, len) }.fill(0.0);
        }
        return if time.is_finite() && time < 0.0 {
            0
        } else {
            -3
        };
    };
    if read_input != 0 {
        // SAFETY: HAL supplies this bounded writable input buffer, and we keep
        // no reference past the callback. Reading never modifies ring storage.
        let output = unsafe { slice::from_raw_parts_mut(data, len) };
        cable.read(timestamp, output);
    } else {
        // SAFETY: WriteMix supplies bounded f32 samples; ring copies atomically.
        let input = unsafe { slice::from_raw_parts(data, len) };
        cable.write(timestamp, input);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_controls_are_writable_metadata_and_never_attenuate_original_pcm() {
        use model::{Value, property::*};
        assert!(matches!(
            model::value(20, CONTROLS, 2, 0, ""),
            Some(Value::List(&[23, 24]))
        ));
        assert!(matches!(
            model::value(20, CONTROLS, 1, 0, ""),
            Some(Value::List(&[]))
        ));
        assert!(matches!(
            model::value(23, OWNER, 0, 0, ""),
            Some(Value::U32(20))
        ));
        assert!(matches!(
            model::value(24, CONTROL_ELEMENT, 0, 0, ""),
            Some(Value::U32(0))
        ));
        assert!(model::value(10, DEVICE_VOLUME, 2, 0, "").is_none());
        assert!(model::value(20, DEVICE_VOLUME, 1, 0, "").is_none());
        assert!(model::value(23, LEVEL_SCALAR, 2, 0, "").is_none());
        assert!(model::value(23, BOOLEAN_VALUE, 0, 0, "").is_none());
        assert_eq!(BabelSetLevel(23, LEVEL_SCALAR, f32::NAN), -3);
        assert_eq!(BabelSetLevel(23, LEVEL_SCALAR, 1.5), -3);
        assert_eq!(BabelSetLevel(23, LEVEL_DECIBELS, -97.0), -3);
        assert_eq!(BabelSetMute(24, 2), -3);
        assert_eq!(BabelSetMute(10, 1), -1);
        assert!(BabelSetLevel(23, LEVEL_SCALAR, 0.25) >= 0);
        assert_eq!(BabelSetLevel(23, LEVEL_SCALAR, 0.25), 0);
        assert!(matches!(
            model::value(20, DEVICE_VOLUME, 2, 0, ""),
            Some(Value::Scalar(0.25))
        ));
        assert!(BabelSetMute(20, 1) >= 0);
        assert!(matches!(
            model::value(24, BOOLEAN_VALUE, 0, 0, ""),
            Some(Value::U32(1))
        ));
        assert_eq!(BabelConvertLevel(1, 0.5), -48.0);
        assert_eq!(BabelConvertLevel(0, -48.0), 0.5);
        assert_eq!(BabelDeviceAction(20, 200, 0), 0);
        assert_eq!(BabelDeviceAction(20, 200, 2), 0);
        let mut written = [0.8, -0.7, 0.6, -0.5];
        let mut captured = [0.0; 4];
        // SAFETY: these fixed f32 arrays contain the stated stereo frame count.
        unsafe {
            assert_eq!(BabelProcess(20, 22, 0, 100.0, 2, written.as_mut_ptr()), 0);
            assert_eq!(BabelProcess(20, 21, 1, 4196.0, 2, captured.as_mut_ptr()), 0);
        }
        assert_eq!(
            captured, written,
            "volume and mute belong to the physical endpoint"
        );
        assert_eq!(BabelDeviceAction(20, 200, 3), 0);
        assert_eq!(BabelDeviceAction(20, 200, 1), 0);
        assert!(BabelSetLevel(20, DEVICE_VOLUME, 1.0) >= 0);
        assert!(BabelSetMute(20, 0) >= 0);
    }
    #[test]
    fn ffi_private_layout_and_audio_buffer_contract() {
        assert_eq!(std::mem::size_of::<BabelProperty>(), 56);
        assert_eq!(std::mem::offset_of!(BabelProperty, number), 40);
        assert_eq!(std::mem::offset_of!(BabelProperty, text), 48);
        assert_eq!(BabelDeviceAction(10, 123, 0), 0);
        assert_eq!(BabelDeviceAction(10, 123, 2), 0);
        let mut write = [0.1, -0.2, 0.3, -0.4];
        let mut read = [0.0; 4];
        // SAFETY: all buffers below are allocated f32 arrays of the stated size.
        unsafe {
            assert_eq!(BabelProcess(10, 12, 0, 100.0, 2, write.as_mut_ptr()), 0);
            assert_eq!(BabelProcess(10, 11, 1, 4196.0, 2, read.as_mut_ptr()), 0);
            assert_eq!(read, write);
            assert_eq!(BabelProcess(10, 11, 1, -512.0, 2, read.as_mut_ptr()), 0);
            assert_eq!(read, [0.0; 4]);
            assert_eq!(BabelProcess(10, 21, 1, 4196.0, 2, read.as_mut_ptr()), -1);
            assert_eq!(BabelProcess(10, 11, 1, 4196.0, 2, ptr::null_mut()), -3);
            assert_eq!(
                BabelProcess(
                    10,
                    11,
                    1,
                    4196.0,
                    (MAX_FRAMES + 1) as u32,
                    read.as_mut_ptr()
                ),
                -3
            );
        }
        assert_eq!(BabelDeviceAction(10, 123, 3), 0);
        assert_eq!(BabelDeviceAction(10, 123, 1), 0);
    }
}
