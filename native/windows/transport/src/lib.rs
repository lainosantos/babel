#![cfg_attr(not(test), no_std)]
#![deny(unsafe_op_in_unsafe_fn)]
mod core;
#[cfg(feature = "kernel")]
mod ffi {
    use super::core::{BYTES, Cable};
    static CABLES: [Cable; 2] = [Cable::new(), Cable::new()];
    #[unsafe(no_mangle)]
    pub extern "C" fn babel_transport_reset(cable: u32) {
        if let Some(cable) = CABLES.get(cable as usize) {
            cable.reset();
        }
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn babel_transport_state(cable: u32, capture: u32, running: u32) {
        if let Some(cable) = CABLES.get(cable as usize) {
            cable.state(capture != 0, running != 0);
        }
    }
    // SAFETY: WDK owns nonpaged DMA buffers for this call, validates their bounds,
    // and serializes each cable at DISPATCH_LEVEL. Never form Rust references to
    // shared DMA memory: user/engine mappings may change independently.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn babel_transport_write(cable: u32, data: *const u8, bytes: usize) {
        let Some(cable) = CABLES.get(cable as usize) else {
            return;
        };
        if data.is_null() || bytes > BYTES || !bytes.is_multiple_of(4) {
            return;
        }
        for offset in (0..bytes).step_by(4) {
            let frame = unsafe {
                u32::from_le_bytes([
                    data.add(offset).read_volatile(),
                    data.add(offset + 1).read_volatile(),
                    data.add(offset + 2).read_volatile(),
                    data.add(offset + 3).read_volatile(),
                ])
            };
            cable.push(frame);
        }
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn babel_transport_read(cable: u32, data: *mut u8, bytes: usize) {
        let Some(cable) = CABLES.get(cable as usize) else {
            return;
        };
        if data.is_null() || bytes > BYTES || !bytes.is_multiple_of(4) {
            return;
        }
        for offset in (0..bytes).step_by(4) {
            let frame = cable.pop().to_le_bytes();
            for (index, byte) in frame.into_iter().enumerate() {
                unsafe {
                    data.add(offset + index).write_volatile(byte);
                }
            }
        }
    }
}
#[cfg(all(not(test), feature = "kernel"))]
#[panic_handler]
fn panic(_: &::core::panic::PanicInfo) -> ! {
    unsafe extern "C" {
        fn BabelTransportPanic() -> !;
    }
    // The WDK shim fails closed using KeBugCheckEx; no unwinding across the ABI.
    unsafe { BabelTransportPanic() }
}

#[cfg(all(test, feature = "kernel"))]
mod abi_tests {
    use super::ffi::*;
    #[test]
    fn abi_round_trip_bounds_and_pair_isolation() {
        for cable in 0..2 {
            babel_transport_reset(cable);
            babel_transport_state(cable, 0, 1);
            babel_transport_state(cable, 1, 1);
        }
        let data = [0xff, 0x7f, 0x00, 0x80, 0x12, 0x34, 0x56, 0x78];
        let mut result = [0xaa; 8];
        unsafe {
            babel_transport_write(0, data.as_ptr(), data.len());
            babel_transport_read(1, result.as_mut_ptr(), result.len());
            assert_eq!(result, [0; 8]);
            babel_transport_read(0, result.as_mut_ptr(), result.len());
            assert_eq!(result, data);
            babel_transport_read(0, result.as_mut_ptr(), 3); // malformed frame size: untouched
            assert_eq!(result, data);
            babel_transport_write(0, ::core::ptr::null(), 4);
            babel_transport_read(0, ::core::ptr::null_mut(), 4);
            babel_transport_read(9, result.as_mut_ptr(), 8);
            assert_eq!(result, data);
        }
    }
}
