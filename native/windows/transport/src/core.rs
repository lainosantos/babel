//! Fixed storage and integer PCM transport. No allocation, pointers, floats or OS calls.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

pub const FRAMES: usize = 4096;
pub const BYTES: usize = FRAMES * 4;
/// Calls on each cable must be serialized by the WDK shim's DISPATCH_LEVEL lock.
/// Atomics keep the backing storage memory safe even if that contract is violated;
/// they do not make a sequence of calls into a lock-free queue transaction.
pub struct Cable {
    frames: [AtomicU32; FRAMES],
    head: AtomicUsize,
    len: AtomicUsize,
    running: AtomicU32,
}
impl Cable {
    pub const fn new() -> Self {
        Self {
            frames: [const { AtomicU32::new(0) }; FRAMES],
            head: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            running: AtomicU32::new(0),
        }
    }
    pub fn reset(&self) {
        self.running.store(0, Relaxed);
        self.clear();
    }
    fn clear(&self) {
        self.head.store(0, Relaxed);
        self.len.store(0, Relaxed);
    }
    pub fn state(&self, capture: bool, running: bool) {
        let bit = if capture { 2 } else { 1 };
        let old = self.running.load(Relaxed);
        let new = if running { old | bit } else { old & !bit };
        if old != new {
            self.clear();
            self.running.store(new, Relaxed);
        }
    }
    pub fn push(&self, frame: u32) {
        if self.running.load(Relaxed) != 3 {
            return;
        }
        let mut head = self.head.load(Relaxed) % FRAMES;
        let mut len = self.len.load(Relaxed).min(FRAMES);
        if len == FRAMES {
            head = (head + 1) % FRAMES;
            len -= 1;
        }
        self.frames[(head + len) % FRAMES].store(frame, Relaxed);
        self.head.store(head, Relaxed);
        self.len.store(len + 1, Relaxed);
    }
    pub fn pop(&self) -> u32 {
        let len = self.len.load(Relaxed).min(FRAMES);
        if self.running.load(Relaxed) != 3 || len == 0 {
            return 0;
        }
        let head = self.head.load(Relaxed) % FRAMES;
        let frame = self.frames[head].load(Relaxed);
        self.head.store((head + 1) % FRAMES, Relaxed);
        self.len.store(len - 1, Relaxed);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn active() -> Cable {
        let cable = Cable::new();
        cable.state(false, true);
        cable.state(true, true);
        cable
    }
    #[test]
    fn pcm_stereo_bits_and_fifo_are_exact() {
        let cable = active();
        for value in [0x7fff8000, 0xffff0001, 0, 0xdeadbeef] {
            cable.push(value);
        }
        for value in [0x7fff8000, 0xffff0001, 0, 0xdeadbeef] {
            assert_eq!(cable.pop(), value);
        }
        assert_eq!(cable.pop(), 0);
    }
    #[test]
    fn overflow_discards_oldest_frames_and_wraps() {
        let cable = active();
        for frame in 1..=FRAMES + 37 {
            cable.push(frame as u32);
        }
        for frame in 38..=FRAMES + 37 {
            assert_eq!(cable.pop(), frame as u32);
        }
        assert_eq!(cable.pop(), 0);
    }
    #[test]
    fn either_side_pausing_flushes_and_never_replays_old_audio() {
        for capture in [false, true] {
            let cable = active();
            cable.push(123);
            cable.state(capture, false);
            cable.push(456);
            assert_eq!(cable.pop(), 0);
            cable.state(capture, true);
            assert_eq!(cable.pop(), 0);
            cable.push(789);
            assert_eq!(cable.pop(), 789);
        }
    }
    #[test]
    fn repeated_run_does_not_drop_audio_and_cables_do_not_mix() {
        let mic = active();
        let speaker = active();
        mic.push(11);
        speaker.push(22);
        mic.state(false, true);
        assert_eq!(mic.pop(), 11);
        assert_eq!(speaker.pop(), 22);
        mic.push(33);
        mic.reset();
        assert_eq!(mic.pop(), 0);
    }
    #[test]
    fn capture_before_render_is_silent() {
        let cable = Cable::new();
        cable.state(true, true);
        cable.push(123);
        assert_eq!(cable.pop(), 0);
        cable.state(false, true);
        assert_eq!(cable.pop(), 0);
    }
}
