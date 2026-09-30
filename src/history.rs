//! Bounded in-memory originals. Capture workers supply mono PCM16 at 16 kHz;
//! this module never opens a device, writes files, or contacts a provider.
use crate::{config::HistoryConfig, recording::RecordingLane};
use serde::Serialize;
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const SAMPLE_RATE: usize = 16_000;
const SAMPLE_NANOS: u64 = 1_000_000_000 / SAMPLE_RATE as u64;
const MAX_FRAME_SAMPLES: usize = SAMPLE_RATE;
// Normal idle routes deliver 100 frames/s. Bound metadata as well as PCM even
// if a producer supplies repeated timestamps or one-sample frames.
const MAX_FRAMES_PER_SECOND: usize = 200;

#[derive(Clone, Debug)]
pub struct HistoryFrame {
    pub lane: RecordingLane,
    /// Shared storage; use samples() because the retained interval may be clipped.
    pub samples: Arc<[i16]>,
    pub captured_at: Instant,
    range: Range<usize>,
}
impl HistoryFrame {
    pub fn samples(&self) -> &[i16] {
        &self.samples[self.range.clone()]
    }

    pub fn started_at(&self) -> Instant {
        self.captured_at - samples_duration(self.range.len())
    }

    fn clip_before(&mut self, cutoff: Instant) {
        let start = self.started_at();
        if cutoff > start {
            let skip = cutoff
                .duration_since(start)
                .as_nanos()
                .div_ceil(u128::from(SAMPLE_NANOS));
            self.range.start += skip.min(self.range.len() as u128) as usize;
        }
    }
}

#[derive(Clone, Debug)]
pub struct HistorySnapshot {
    /// First retained sample, or `now` when there are no frames. Gaps between
    /// this point and session start remain represented by the frame timestamps.
    pub origin: Instant,
    pub frames: Vec<HistoryFrame>,
    pub included_secs: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryStatus {
    pub enabled: bool,
    pub capacity_secs: u32,
    /// Wall-clock coverage up to now, including gaps after/between captures.
    pub available_secs: f64,
    /// Retained PCM durations per lane, excluding gaps.
    pub microphone_secs: f64,
    pub speaker_secs: f64,
    pub buffered_bytes: usize,
}
impl Default for HistoryStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            capacity_secs: HistoryConfig::default().duration_secs,
            available_secs: 0.0,
            microphone_secs: 0.0,
            speaker_secs: 0.0,
            buffered_bytes: 0,
        }
    }
}

#[derive(Default)]
struct Lane {
    frames: VecDeque<HistoryFrame>,
    allocated_samples: usize,
}
impl Lane {
    fn pop_front(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.allocated_samples -= frame.samples.len();
        }
    }

    fn trim(&mut self, cutoff: Instant, sample_cap: usize, frame_cap: usize) {
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.captured_at <= cutoff)
        {
            self.pop_front();
        }
        for frame in self.frames.iter_mut().take_while(|frame| {
            frame.captured_at.saturating_duration_since(cutoff) < Duration::from_secs(1)
        }) {
            frame.clip_before(cutoff);
            if frame.range.start > 0 {
                // A buffer boundary may release its expired prefix; snapshots
                // keep sharing their old allocation without changing contents.
                // Count retained allocations, so a partial boundary cannot make
                // the sample cap discard the remainder of an otherwise useful frame.
                let kept: Arc<[i16]> = Arc::from(frame.samples());
                self.allocated_samples -= frame.samples.len() - kept.len();
                frame.range = 0..kept.len();
                frame.samples = kept;
            }
        }
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.samples().is_empty())
        {
            self.pop_front();
        }
        while self.allocated_samples > sample_cap || self.frames.len() > frame_cap {
            self.pop_front();
        }
    }
}

struct Contents {
    config: HistoryConfig,
    lanes: [Lane; 2],
    pruned_at: Option<Instant>,
}
impl Contents {
    fn prune(&mut self, now: Instant) {
        let now = self.pruned_at.map_or(now, |previous| now.max(previous));
        self.pruned_at = Some(now);
        if !self.config.enabled {
            self.lanes = Default::default();
            return;
        }
        let capacity = self.config.duration_secs as usize;
        let cutoff = now
            .checked_sub(Duration::from_secs(capacity as u64))
            .unwrap_or(now);
        for lane in &mut self.lanes {
            lane.trim(
                cutoff,
                capacity * SAMPLE_RATE,
                capacity * MAX_FRAMES_PER_SECOND,
            );
        }
    }

    fn first_sample(&self) -> Option<Instant> {
        self.lanes
            .iter()
            .flat_map(|lane| &lane.frames)
            .map(HistoryFrame::started_at)
            .min()
    }
}

pub struct HistoryBuffer {
    enabled: AtomicBool,
    contents: Mutex<Contents>,
}
impl HistoryBuffer {
    pub fn new(config: &HistoryConfig) -> Self {
        Self {
            enabled: AtomicBool::new(config.enabled),
            contents: Mutex::new(Contents {
                config: bounded_config(config),
                lanes: Default::default(),
                pruned_at: None,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Reducing retention prunes immediately; disabling releases all stored PCM.
    /// A larger capacity preserves available originals without manufacturing history.
    pub fn configure(&self, config: &HistoryConfig) {
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        contents.config = bounded_config(config);
        contents.prune(Instant::now());
        self.enabled.store(config.enabled, Ordering::Relaxed);
    }

    /// Called on a worker, never an audio callback. Invalid/oversized frames and
    /// per-lane timestamps moving backwards are dropped before allocating PCM.
    pub fn push(&self, lane: RecordingLane, samples: &[i16], captured_at: Instant) {
        if !self.enabled() || samples.is_empty() || samples.len() > MAX_FRAME_SAMPLES {
            return;
        }
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        if !contents.config.enabled {
            return;
        }
        contents.prune(captured_at);
        let lane_buffer = &mut contents.lanes[lane_index(lane)];
        if lane_buffer
            .frames
            .back()
            .is_some_and(|last| captured_at < last.captured_at)
        {
            return;
        }
        lane_buffer.allocated_samples += samples.len();
        lane_buffer.frames.push_back(HistoryFrame {
            lane,
            samples: Arc::from(samples),
            captured_at,
            range: 0..samples.len(),
        });
        contents.prune(captured_at);
    }

    /// The routing monitor calls this even while capture is idle, so elapsed
    /// wall time releases stale audio without needing another frame or snapshot.
    pub fn prune(&self, now: Instant) {
        self.contents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prune(now);
    }

    pub fn snapshot(&self, requested_seconds: u32, now: Instant) -> HistorySnapshot {
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        contents.prune(now);
        let duration = requested_seconds.min(contents.config.duration_secs);
        let cutoff = now
            .checked_sub(Duration::from_secs(u64::from(duration)))
            .unwrap_or(now);
        let mut frames = contents
            .lanes
            .iter()
            .flat_map(|lane| &lane.frames)
            .filter(|frame| frame.captured_at > cutoff && frame.captured_at <= now)
            .cloned()
            .filter_map(|mut frame| {
                frame.clip_before(cutoff);
                (!frame.samples().is_empty()).then_some(frame)
            })
            .collect::<Vec<_>>();
        drop(contents);
        // Globally chronological completion keeps a mixed recording's holdback
        // bounded while preserving order within both independent capture lanes.
        frames.sort_by_key(|frame| frame.captured_at);
        let origin = frames
            .iter()
            .map(HistoryFrame::started_at)
            .min()
            .unwrap_or(now);
        HistorySnapshot {
            origin,
            frames,
            included_secs: now.saturating_duration_since(origin).as_secs_f64(),
        }
    }

    pub fn status(&self, now: Instant) -> HistoryStatus {
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        contents.prune(now);
        let seconds = |lane: &Lane| {
            lane.frames
                .iter()
                .map(|frame| frame.samples().len())
                .sum::<usize>() as f64
                / SAMPLE_RATE as f64
        };
        HistoryStatus {
            enabled: contents.config.enabled,
            capacity_secs: contents.config.duration_secs,
            available_secs: contents.first_sample().map_or(0.0, |start| {
                now.saturating_duration_since(start).as_secs_f64()
            }),
            microphone_secs: seconds(&contents.lanes[0]),
            speaker_secs: seconds(&contents.lanes[1]),
            buffered_bytes: contents
                .lanes
                .iter()
                .map(|lane| lane.allocated_samples * size_of::<i16>())
                .sum(),
        }
    }
}

fn bounded_config(config: &HistoryConfig) -> HistoryConfig {
    HistoryConfig {
        enabled: config.enabled,
        duration_secs: config.duration_secs.clamp(1, 3600),
    }
}
fn lane_index(lane: RecordingLane) -> usize {
    match lane {
        RecordingLane::Microphone => 0,
        RecordingLane::Speaker => 1,
    }
}
fn samples_duration(samples: usize) -> Duration {
    Duration::from_nanos(samples as u64 * SAMPLE_NANOS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use RecordingLane::{Microphone, Speaker};

    fn buffer(seconds: u32) -> HistoryBuffer {
        HistoryBuffer::new(&HistoryConfig {
            enabled: true,
            duration_secs: seconds,
        })
    }

    #[test]
    fn rolling_history_expires_by_wall_clock_even_without_more_capture() {
        let history = buffer(10);
        let now = Instant::now();
        history.push(Microphone, &[1; 160], now - Duration::from_secs(11));
        history.push(Speaker, &[2; 160], now - Duration::from_secs(2));
        let snapshot = history.snapshot(10, now);
        assert_eq!(snapshot.frames.len(), 1);
        assert_eq!(snapshot.frames[0].lane, Speaker);
        assert_eq!(snapshot.included_secs, 2.01);
        history.prune(now + Duration::from_secs(11));
        assert_eq!(
            history.status(now + Duration::from_secs(11)).buffered_bytes,
            0
        );
        assert!(
            history
                .snapshot(10, now + Duration::from_secs(11))
                .frames
                .is_empty()
        );
        history.push(Microphone, &[99; 160], now);
        assert_eq!(
            history.status(now + Duration::from_secs(11)).buffered_bytes,
            0
        );
    }

    #[test]
    fn retention_clips_partial_frames_without_discarding_the_remaining_audio() {
        let history = buffer(1);
        let now = Instant::now();
        history.push(
            Microphone,
            &vec![1; SAMPLE_RATE],
            now - Duration::from_millis(250),
        );
        history.push(Microphone, &[2; 4000], now);
        let snapshot = history.snapshot(1, now);
        assert_eq!(snapshot.origin, now - Duration::from_secs(1));
        assert_eq!(snapshot.frames.len(), 2);
        assert_eq!(snapshot.frames[0].samples().len(), 12000);
        assert_eq!(snapshot.frames[1].samples().len(), 4000);
        assert_eq!(
            history.status(now).buffered_bytes,
            SAMPLE_RATE * size_of::<i16>()
        );
    }

    #[test]
    fn snapshots_clip_at_sample_precision_share_storage_and_preserve_lane_gaps() {
        let history = buffer(10);
        let now = Instant::now();
        let first_end = now - Duration::from_millis(1500);
        history.push(Microphone, &vec![101; SAMPLE_RATE], first_end);
        history.push(Speaker, &[202; 160], now - Duration::from_millis(400));
        history.push(Microphone, &[0; 160], now);
        let whole = history.snapshot(10, now);
        let clipped = history.snapshot(2, now);
        assert_eq!(clipped.origin, now - Duration::from_secs(2));
        assert_eq!(clipped.included_secs, 2.0);
        assert_eq!(clipped.frames[0].samples().len(), 8000);
        assert_eq!(clipped.frames[0].captured_at, first_end);
        assert!(Arc::ptr_eq(
            &whole.frames[0].samples,
            &clipped.frames[0].samples
        ));
        assert_eq!(
            clipped.frames[1].started_at(),
            now - Duration::from_millis(410)
        );
        assert_eq!(clipped.frames[2].samples(), &[0; 160]);
        assert!(history.snapshot(0, now).frames.is_empty());
    }

    #[test]
    fn shrinking_capacity_trims_and_disabling_discards_originals() {
        let history = buffer(600);
        let now = Instant::now();
        history.push(Microphone, &[1; 160], now - Duration::from_secs(20));
        history.push(Speaker, &[2; 160], now - Duration::from_secs(1));
        history.configure(&HistoryConfig {
            enabled: true,
            duration_secs: 10,
        });
        assert_eq!(history.snapshot(600, Instant::now()).frames.len(), 1);
        history.configure(&HistoryConfig {
            enabled: true,
            duration_secs: 600,
        });
        assert_eq!(history.snapshot(600, Instant::now()).frames.len(), 1);
        history.configure(&HistoryConfig {
            enabled: false,
            duration_secs: 600,
        });
        history.push(Microphone, &[3; 160], Instant::now());
        assert_eq!(history.status(Instant::now()).buffered_bytes, 0);
        assert!(!history.enabled());
        history.configure(&HistoryConfig::default());
        assert!(history.snapshot(600, Instant::now()).frames.is_empty());
    }

    #[test]
    fn sample_and_metadata_caps_hold_under_duplicate_timestamps() {
        let history = buffer(1);
        let now = Instant::now();
        let second = vec![7; SAMPLE_RATE];
        for _ in 0..10 {
            history.push(Microphone, &second, now);
            history.push(Speaker, &second, now);
        }
        assert_eq!(
            history.status(now).buffered_bytes,
            2 * SAMPLE_RATE * size_of::<i16>()
        );
        assert_eq!(history.snapshot(1, now).frames.len(), 2);
        for _ in 0..1000 {
            history.push(Microphone, &[8], now);
            history.push(Speaker, &[8], now);
        }
        assert_eq!(
            history.snapshot(1, now).frames.len(),
            2 * MAX_FRAMES_PER_SECOND
        );
        assert!(history.status(now).buffered_bytes <= 2 * SAMPLE_RATE * size_of::<i16>());
    }

    #[test]
    fn chronological_snapshot_merges_lanes_and_rejects_oversized_or_backwards_frames() {
        let history = buffer(10);
        let now = Instant::now();
        history.push(Speaker, &[3; 160], now);
        history.push(Microphone, &[1; 160], now - Duration::from_secs(2));
        history.push(Microphone, &[2; 160], now - Duration::from_secs(1));
        history.push(Microphone, &[99; 160], now - Duration::from_secs(3));
        history.push(Speaker, &vec![99; SAMPLE_RATE + 1], now);
        history.push(Speaker, &[], now);
        let snapshot = history.snapshot(10, now);
        assert_eq!(
            snapshot
                .frames
                .iter()
                .map(|frame| frame.samples()[0])
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(history.status(now).microphone_secs, 0.02);
        assert_eq!(history.status(now).speaker_secs, 0.01);
    }
}
