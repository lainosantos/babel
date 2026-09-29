//! Fixed-capacity, timestamp-addressed stereo loopback shared by the two HAL cables.
//! No heap allocation, waiting, OS calls, or unsafe code occurs in sample/clock processing.
//! A short control-only mutex serializes client registration and StartIO/StopIO.
#![forbid(unsafe_code)]
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const CAPACITY: usize = 16_384;
pub const MAX_FRAMES: usize = 4096;
pub const LATENCY_FRAMES: u64 = MAX_FRAMES as u64;
pub const TIMESTAMP_PERIOD: u64 = 512;
pub const MAX_CLIENTS: usize = 256;
pub const MICROPHONE_UID: &str = "org.babel.audio.microphone.v1";
pub const SPEAKER_UID: &str = "org.babel.audio.speaker.v1";
const INVALID: u64 = u64::MAX;
const _: () = assert!(CAPACITY.is_power_of_two());
const _: () = assert!(LATENCY_FRAMES >= MAX_FRAMES as u64);
const _: () = assert!(CAPACITY >= MAX_FRAMES * 2 + LATENCY_FRAMES as usize);
#[cfg(not(target_has_atomic = "64"))]
compile_error!("The Babel audio callback requires native 64-bit atomics");

struct Frame {
    stamp: AtomicU64,
    generation: AtomicU64,
    stereo: AtomicU64,
}
impl Frame {
    const fn new() -> Self {
        Self {
            stamp: AtomicU64::new(INVALID),
            generation: AtomicU64::new(0),
            stereo: AtomicU64::new(0),
        }
    }
}

pub struct Cable {
    frames: [Frame; CAPACITY],
    // WriteMix should have one HAL producer; accidental concurrent producers
    // drop their block instead of blocking or publishing a torn stereo frame.
    writing: AtomicBool,
    // Control callbacks only. Neither audio processing nor the clock takes it.
    control: Mutex<()>,
    generation: AtomicU64,
    clients: [AtomicU64; MAX_CLIENTS],
    input_active: AtomicBool,
    output_active: AtomicBool,
}
impl Default for Cable {
    fn default() -> Self {
        Self::new()
    }
}
impl Cable {
    pub const fn new() -> Self {
        Self {
            frames: [const { Frame::new() }; CAPACITY],
            writing: AtomicBool::new(false),
            control: Mutex::new(()),
            generation: AtomicU64::new(1),
            clients: [const { AtomicU64::new(0) }; MAX_CLIENTS],
            input_active: AtomicBool::new(true),
            output_active: AtomicBool::new(true),
        }
    }
    fn identity(id: u32) -> u64 {
        (u64::from(id) + 1) << 1
    }
    pub fn add_client(&self, id: u32) -> bool {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let identity = Self::identity(id);
        if self
            .clients
            .iter()
            .any(|slot| slot.load(SeqCst) & !1 == identity)
        {
            return true;
        }
        self.clients
            .iter()
            .any(|slot| slot.compare_exchange(0, identity, SeqCst, SeqCst).is_ok())
    }
    pub fn remove_client(&self, id: u32) -> bool {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let identity = Self::identity(id);
        for slot in &self.clients {
            let state = slot.load(SeqCst);
            if state & !1 == identity && slot.compare_exchange(state, 0, SeqCst, SeqCst).is_ok() {
                if state & 1 != 0 {
                    self.stopped();
                }
                return true;
            }
        }
        false
    }
    pub fn start(&self, id: u32) -> bool {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let identity = Self::identity(id);
        for slot in &self.clients {
            let state = slot.load(SeqCst);
            if state == identity | 1 {
                return true;
            }
            if state == identity
                && slot
                    .compare_exchange(identity, identity | 1, SeqCst, SeqCst)
                    .is_ok()
            {
                return true;
            }
        }
        false
    }
    pub fn stop(&self, id: u32) -> bool {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        let identity = Self::identity(id);
        for slot in &self.clients {
            let state = slot.load(SeqCst);
            if state == identity {
                return true;
            }
            if state == identity | 1
                && slot
                    .compare_exchange(state, identity, SeqCst, SeqCst)
                    .is_ok()
            {
                self.stopped();
                return true;
            }
        }
        false
    }
    fn stopped(&self) {
        if !self.is_running() {
            self.invalidate();
        }
    }
    pub fn is_running(&self) -> bool {
        // The slots are the sole source of truth; a separate counter could
        // underflow when StopIO races StartIO after its client CAS. This scan is
        // bounded to 256 atomics and normally exits at the first active client.
        self.clients.iter().any(|slot| slot.load(SeqCst) & 1 != 0)
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(SeqCst)
    }
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, SeqCst);
    }
    pub fn active(&self, input: bool) -> bool {
        if input {
            self.input_active.load(SeqCst)
        } else {
            self.output_active.load(SeqCst)
        }
    }
    pub fn set_active(&self, input: bool, active: bool) -> bool {
        let previous = if input {
            self.input_active.swap(active, SeqCst)
        } else {
            self.output_active.swap(active, SeqCst)
        };
        if previous != active {
            self.invalidate();
        }
        previous != active
    }
    fn valid_buffer(start: u64, samples: usize) -> bool {
        samples % CHANNELS == 0
            && samples / CHANNELS <= MAX_FRAMES
            && start
                .checked_add((samples / CHANNELS) as u64)
                .is_some_and(|end| end < INVALID)
    }
    pub fn write(&self, start: u64, samples: &[f32]) -> bool {
        if !Self::valid_buffer(start, samples.len())
            || !self.is_running()
            || !self.active(false)
            || self
                .writing
                .compare_exchange(false, true, SeqCst, SeqCst)
                .is_err()
        {
            return false;
        }
        let generation = self.generation();
        for (offset, stereo) in samples.chunks_exact(CHANNELS).enumerate() {
            let timestamp = start + offset as u64;
            let frame = &self.frames[timestamp as usize & (CAPACITY - 1)];
            let clean = |value: f32| {
                if value.is_finite() {
                    value.to_bits()
                } else {
                    0
                }
            };
            let packed = u64::from(clean(stereo[0])) | u64::from(clean(stereo[1])) << 32;
            // All atomic operations are sequentially consistent: readers cannot
            // observe new data between two observations of an old valid stamp.
            frame.stamp.store(INVALID, SeqCst);
            frame.stereo.store(packed, SeqCst);
            frame.generation.store(generation, SeqCst);
            frame.stamp.store(timestamp, SeqCst);
        }
        self.writing.store(false, SeqCst);
        true
    }
    pub fn read(&self, input_time: u64, output: &mut [f32]) -> bool {
        output.fill(0.0);
        if !Self::valid_buffer(input_time, output.len()) || !self.is_running() || !self.active(true)
        {
            return false;
        }
        let generation = self.generation();
        for (offset, stereo) in output.chunks_exact_mut(CHANNELS).enumerate() {
            let Some(timestamp) = (input_time + offset as u64).checked_sub(LATENCY_FRAMES) else {
                continue;
            };
            let frame = &self.frames[timestamp as usize & (CAPACITY - 1)];
            let before = frame.stamp.load(SeqCst);
            let frame_generation = frame.generation.load(SeqCst);
            let value = frame.stereo.load(SeqCst);
            let after = frame.stamp.load(SeqCst);
            if before == timestamp && after == timestamp && frame_generation == generation {
                stereo[0] = f32::from_bits(value as u32);
                stereo[1] = f32::from_bits((value >> 32) as u32);
            }
        }
        if self.generation() != generation || !self.is_running() {
            output.fill(0.0);
        }
        true
    }
}

/// mach_absolute_time is converted with integer arithmetic, so quantized clock
/// timestamps remain stable and aligned without an RT timer, lock or drift loop.
pub fn zero_timestamp(anchor: u64, now: u64, numer: u32, denom: u32) -> Option<(u64, u64)> {
    if numer == 0 || denom == 0 || now < anchor {
        return None;
    }
    let elapsed = u128::from(now - anchor);
    let frames =
        elapsed * u128::from(numer) * u128::from(SAMPLE_RATE) / (u128::from(denom) * 1_000_000_000);
    let aligned = frames / u128::from(TIMESTAMP_PERIOD) * u128::from(TIMESTAMP_PERIOD);
    let ticks =
        aligned * u128::from(denom) * 1_000_000_000 / (u128::from(numer) * u128::from(SAMPLE_RATE));
    Some((
        u64::try_from(aligned).ok()?,
        anchor.checked_add(u64::try_from(ticks).ok()?)?,
    ))
}

pub fn sample_time(value: f64) -> Option<u64> {
    // HAL sample timestamps are whole frames. Never cast NaN/negative/huge values.
    (value.is_finite()
        && (0.0..=(1_u64 << 52) as f64).contains(&value)
        && value.fract().abs() < 0.001)
        .then_some(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cable() -> Box<Cable> {
        // Tests allocate their fixtures; production stores both cables statically.
        let cable = Box::new(Cable::new());
        assert!(cable.add_client(7));
        assert!(cable.start(7));
        cable
    }
    #[test]
    fn two_cables_and_multiple_readers_are_independent() {
        let mic = cable();
        let speaker = cable();
        let data = [0.25, -0.5, 0.75, -1.0];
        assert!(mic.write(100, &data));
        let mut one = [0.0; 4];
        let mut two = [0.0; 4];
        assert!(mic.read(100 + LATENCY_FRAMES, &mut one));
        assert!(mic.read(100 + LATENCY_FRAMES, &mut two));
        assert_eq!(one, data);
        assert_eq!(two, data);
        speaker.read(100 + LATENCY_FRAMES, &mut two);
        assert_eq!(two, [0.0; 4]);
    }
    #[test]
    fn overwritten_missing_and_stopped_audio_is_silent() {
        let cable = cable();
        let mut out = [9.0; 4];
        cable.write(100, &[0.2; 4]);
        cable.write(100 + CAPACITY as u64, &[0.7; 4]);
        cable.read(100 + LATENCY_FRAMES, &mut out);
        assert_eq!(out, [0.0; 4]);
        assert!(cable.stop(7));
        assert!(cable.start(7));
        cable.read(100 + CAPACITY as u64 + LATENCY_FRAMES, &mut out);
        assert_eq!(out, [0.0; 4]);
        cable.write(20, &[f32::NAN, f32::INFINITY, 0.1, -0.1]);
        cable.read(20 + LATENCY_FRAMES, &mut out);
        assert_eq!(out, [0.0, 0.0, 0.1, -0.1]);
    }
    #[test]
    fn boundaries_wrap_and_admission_fail_without_partial_frames() {
        let cable = cable();
        let start = CAPACITY as u64 - 1;
        let samples = [0.1, 0.2, 0.3, 0.4];
        assert!(cable.write(start, &samples));
        let mut out = [0.0; 4];
        cable.read(start + LATENCY_FRAMES, &mut out);
        assert_eq!(out, samples);
        assert!(!cable.write(0, &[1.0]));
        assert!(!cable.write(u64::MAX - 1, &samples));
        cable.writing.store(true, SeqCst);
        assert!(!cable.write(0, &samples));
        cable.writing.store(false, SeqCst);
        assert!(cable.set_active(false, false));
        assert!(!cable.write(0, &samples));
        cable.read(start + LATENCY_FRAMES, &mut out);
        assert_eq!(out, [0.0; 4]);
    }
    #[test]
    fn client_lifetime_is_bounded_and_duplicate_stop_does_not_underflow() {
        let cable = Cable::new();
        for id in 0..MAX_CLIENTS as u32 {
            assert!(cable.add_client(id));
        }
        assert!(!cable.add_client(MAX_CLIENTS as u32));
        assert!(cable.start(0));
        assert!(cable.start(0));
        assert!(cable.start(1));
        assert!(cable.stop(0));
        assert!(cable.stop(0));
        assert!(cable.is_running());
        assert!(cable.remove_client(1));
        assert!(!cable.is_running());
        assert!(!cable.stop(1));
        assert!(cable.add_client(u32::MAX));
        assert!(cable.start(u32::MAX));
    }
    #[test]
    fn maximum_blocks_read_before_write_preserve_the_previous_complete_block() {
        let cable = cable();
        let mut source = vec![0.0; MAX_FRAMES * CHANNELS];
        let mut output = vec![0.0; MAX_FRAMES * CHANNELS];
        for block in 0..8_u64 {
            let start = block * MAX_FRAMES as u64;
            assert!(cable.read(start, &mut output));
            for (frame, stereo) in output.chunks_exact(CHANNELS).enumerate() {
                let expected = if block == 0 {
                    0.0
                } else {
                    (start - LATENCY_FRAMES + frame as u64 + 1) as f32
                };
                assert_eq!(stereo, [expected, -expected]);
            }
            for (frame, stereo) in source.chunks_exact_mut(CHANNELS).enumerate() {
                let value = (start + frame as u64 + 1) as f32;
                stereo.copy_from_slice(&[value, -value]);
            }
            assert!(cable.write(start, &source));
        }
    }
    #[test]
    fn concurrent_registration_of_the_same_client_never_leaves_a_ghost() {
        let cable = std::sync::Arc::new(Cable::new());
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let cable = cable.clone();
                scope.spawn(move || {
                    assert!(cable.add_client(42));
                    assert!(cable.start(42));
                });
            }
        });
        assert_eq!(
            cable
                .clients
                .iter()
                .filter(|slot| slot.load(SeqCst) & !1 == Cable::identity(42))
                .count(),
            1
        );
        assert!(cable.remove_client(42));
        assert!(!cable.is_running());
    }
    #[test]
    fn zero_clock_matches_intel_and_apple_silicon_timebases() {
        for (numer, denom) in [(1, 1), (125, 3)] {
            let anchor = 500;
            let ticks = 1_000_000_000_u64 * u64::from(denom) / u64::from(numer);
            let (sample, host) = zero_timestamp(anchor, anchor + ticks, numer, denom).unwrap();
            assert_eq!(sample, 47_616);
            assert!(host <= anchor + ticks);
            let next = zero_timestamp(anchor, anchor + ticks + 1, numer, denom).unwrap();
            assert!(next.0 >= sample);
        }
        assert!(zero_timestamp(5, 4, 1, 1).is_none());
        assert!(zero_timestamp(0, 1, 0, 1).is_none());
        for value in [f64::NAN, f64::INFINITY, -1.0, 1.5] {
            assert!(sample_time(value).is_none());
        }
    }
    #[test]
    fn concurrent_readers_never_observe_cross_frame_or_torn_stereo() {
        let cable = std::sync::Arc::new(Cable::new());
        cable.add_client(1);
        cable.start(1);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let cable = cable.clone();
                scope.spawn(move || {
                    let mut out = [0.0; 2];
                    for frame in 0..100_000_u64 {
                        cable.read(frame + LATENCY_FRAMES, &mut out);
                        assert!(out == [0.0; 2] || out == [frame as f32, -(frame as f32)]);
                    }
                });
            }
            for frame in 0..100_000_u64 {
                cable.write(frame, &[frame as f32, -(frame as f32)]);
            }
        });
    }
}
