//! Rolling originals with bounded memory staging and encrypted disk storage.
//! Capture workers supply mono PCM16 at 16 kHz; only retention workers perform
//! disk I/O. Snapshots pin encrypted references and decrypt incrementally.
mod storage;
use crate::{config::HistoryConfig, recording::RecordingLane};
use serde::Serialize;
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
pub use storage::Reader as HistoryReader;

const SAMPLE_RATE: usize = 16_000;
const SAMPLE_NANOS: u64 = 1_000_000_000 / SAMPLE_RATE as u64;
const MAX_FRAME_SAMPLES: usize = SAMPLE_RATE;
// Normal idle routes deliver 100 frames/s. Bound metadata as well as PCM even
// if a producer supplies repeated timestamps or one-sample frames.
const MAX_FRAMES_PER_SECOND: usize = 200;

#[derive(Clone, Debug)]
pub struct HistoryFrame {
    pub lane: RecordingLane,
    /// Shared storage; use read_samples() because the retained interval may be clipped.
    pub samples: Arc<storage::Audio>,
    pub captured_at: Instant,
    range: Range<usize>,
}
impl HistoryFrame {
    pub fn sample_count(&self) -> usize {
        self.range.len()
    }

    pub fn memory_bytes(&self) -> usize {
        self.samples.memory_bytes()
    }

    pub async fn read_samples(
        &self,
        reader: &mut HistoryReader,
    ) -> anyhow::Result<zeroize::Zeroizing<Vec<i16>>> {
        reader.samples(&self.samples, self.range.clone()).await
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
    /// Union of the sample timelines reconstructed for mixed recording: real
    /// gaps are excluded, callback jitter is absorbed, and simultaneous lanes
    /// count once. Original PCM chunks within a lane remain consecutive.
    pub combined_audio_secs: f64,
    /// Retained PCM durations per lane, excluding gaps.
    pub microphone_secs: f64,
    pub speaker_secs: f64,
    /// Actual plaintext staging, independent of the configured rolling duration.
    pub buffered_bytes: usize,
    pub retained_bytes: usize,
    pub encrypted_bytes: usize,
    pub storage_error: Option<String>,
    pub dropped_frames: u64,
}
impl Default for HistoryStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            capacity_secs: HistoryConfig::default().duration_secs,
            available_secs: 0.0,
            combined_audio_secs: 0.0,
            microphone_secs: 0.0,
            speaker_secs: 0.0,
            buffered_bytes: 0,
            retained_bytes: 0,
            encrypted_bytes: 0,
            storage_error: None,
            dropped_frames: 0,
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
            self.allocated_samples -= frame.sample_count();
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
            let previous = frame.sample_count();
            frame.clip_before(cutoff);
            self.allocated_samples -= previous - frame.sample_count();
        }
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.sample_count() == 0)
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
    enabled_since: Option<Instant>,
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

    fn combined_audio_duration(&self) -> Duration {
        let Some(origin) = self.first_sample() else {
            return Duration::ZERO;
        };
        // Mirror SessionAudioRecorder::append: callback/pipe batches may stamp
        // several consecutive PCM frames with almost the same wall clock. Each
        // lane advances by its sample count; scheduler jitter up to 50 ms does
        // not create gaps or make those original samples overlap themselves.
        // Reconstructed starts are monotonic, permitting a forward merge with
        // constant memory and no sorting or allocations in the status path.
        fn intervals(lane: &Lane, origin: Instant) -> impl Iterator<Item = (u64, u64)> + '_ {
            let mut expected: Option<u64> = None;
            lane.frames.iter().map(move |frame| {
                let end_by_clock = (frame
                    .captured_at
                    .saturating_duration_since(origin)
                    .as_nanos()
                    * SAMPLE_RATE as u128
                    / 1_000_000_000) as u64;
                let samples = frame.sample_count() as u64;
                let candidate = end_by_clock.saturating_sub(samples);
                let start = match expected {
                    Some(next) if candidate <= next.saturating_add(SAMPLE_RATE as u64 / 20) => next,
                    _ => candidate,
                };
                let end = start + samples;
                expected = Some(end);
                (start, end)
            })
        }
        let mut microphone = intervals(&self.lanes[0], origin).peekable();
        let mut speaker = intervals(&self.lanes[1], origin).peekable();
        let mut interval: Option<(u64, u64)> = None;
        let mut total = 0_u64;
        loop {
            let next = match (microphone.peek(), speaker.peek()) {
                (Some(mic), Some(output)) if mic.0 <= output.0 => microphone.next(),
                (Some(_), Some(_)) => speaker.next(),
                (Some(_), None) => microphone.next(),
                (None, Some(_)) => speaker.next(),
                (None, None) => break,
            };
            let Some((start, end)) = next else { break };
            interval = Some(match interval {
                Some((earliest, latest)) if start <= latest => (earliest, latest.max(end)),
                Some((earliest, latest)) => {
                    total += latest - earliest;
                    (start, end)
                }
                None => (start, end),
            });
        }
        if let Some((start, end)) = interval {
            total += end - start;
        }
        Duration::from_nanos(total * SAMPLE_NANOS)
    }
}

pub struct HistoryBuffer {
    enabled: AtomicBool,
    generation: AtomicU64,
    contents: Mutex<Contents>,
    storage: Option<storage::Storage>,
    storage_error: Option<String>,
}
impl HistoryBuffer {
    pub fn new(config: &HistoryConfig) -> Self {
        let (storage, storage_error) = match storage::Storage::new() {
            Ok(storage) => (Some(storage), None),
            Err(_) => (
                None,
                Some("Recent audio encrypted storage could not start".into()),
            ),
        };
        Self {
            storage,
            storage_error,
            enabled: AtomicBool::new(config.enabled),
            generation: AtomicU64::new(0),
            contents: Mutex::new(Contents {
                config: bounded_config(config),
                lanes: Default::default(),
                pruned_at: None,
                enabled_since: None,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Workers discard queued captures from before the latest explicit enable.
    pub fn accepts(&self, captured_at: Instant) -> bool {
        if !self.enabled() {
            return false;
        }
        let contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        contents.config.enabled
            && contents
                .enabled_since
                .is_none_or(|since| captured_at >= since)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Reducing retention prunes immediately; disabling releases all stored PCM.
    /// A larger capacity preserves available originals without manufacturing history.
    pub fn configure(&self, config: &HistoryConfig) {
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        if contents.config.enabled != config.enabled {
            if let Some(storage) = &self.storage {
                storage.reset_gaps();
            }
            contents.enabled_since = config.enabled.then(Instant::now);
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
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
        if !contents.config.enabled
            || contents
                .enabled_since
                .is_some_and(|since| captured_at < since)
        {
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
        let Some(audio) = self
            .storage
            .as_ref()
            .and_then(|storage| storage.stage(samples))
        else {
            return;
        };
        lane_buffer.allocated_samples += samples.len();
        lane_buffer.frames.push_back(HistoryFrame {
            lane,
            samples: audio,
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
                (frame.sample_count() > 0).then_some(frame)
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

    #[cfg(test)]
    pub(crate) async fn flush(&self) {
        if let Some(storage) = &self.storage {
            storage.flush().await;
        }
    }

    pub fn status(&self, now: Instant) -> HistoryStatus {
        let mut contents = self.contents.lock().unwrap_or_else(|e| e.into_inner());
        contents.prune(now);
        let seconds = |lane: &Lane| {
            lane.frames
                .iter()
                .map(|frame| frame.sample_count())
                .sum::<usize>() as f64
                / SAMPLE_RATE as f64
        };
        if let Some(storage) = &self.storage {
            storage.expire_gaps(
                now.checked_sub(Duration::from_secs(u64::from(
                    contents.config.duration_secs,
                )))
                .unwrap_or(now),
            );
        }
        let (memory_bytes, storage_error, dropped_frames) = self
            .storage
            .as_ref()
            .map_or((0, self.storage_error.clone(), 0), storage::Storage::status);
        let retained_bytes = contents
            .lanes
            .iter()
            .map(|lane| lane.allocated_samples * 2)
            .sum();
        let encrypted_bytes = contents
            .lanes
            .iter()
            .flat_map(|lane| &lane.frames)
            .filter(|frame| frame.memory_bytes() == 0)
            .map(|frame| frame.sample_count() * 2)
            .sum();
        HistoryStatus {
            enabled: contents.config.enabled,
            capacity_secs: contents.config.duration_secs,
            available_secs: contents.first_sample().map_or(0.0, |start| {
                now.saturating_duration_since(start).as_secs_f64()
            }),
            combined_audio_secs: contents.combined_audio_duration().as_secs_f64(),
            microphone_secs: seconds(&contents.lanes[0]),
            speaker_secs: seconds(&contents.lanes[1]),
            buffered_bytes: memory_bytes,
            retained_bytes,
            encrypted_bytes,
            storage_error,
            dropped_frames,
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

    #[test]
    fn reenabled_history_rejects_old_processing_queue_frames() {
        let config = HistoryConfig::default();
        let history = HistoryBuffer::new(&config);
        let queued_at = Instant::now();
        history.configure(&HistoryConfig {
            enabled: false,
            ..config.clone()
        });
        history.configure(&config);
        assert_eq!(history.generation(), 2);
        assert!(!history.accepts(queued_at));
        history.push(RecordingLane::Microphone, &[1; 160], queued_at);
        assert!(history.snapshot(600, Instant::now()).frames.is_empty());
        let current = Instant::now();
        assert!(history.accepts(current));
        history.push(RecordingLane::Microphone, &[2; 160], current);
        assert_eq!(history.snapshot(600, Instant::now()).frames.len(), 1);
    }
    use RecordingLane::{Microphone, Speaker};

    fn buffer(seconds: u32) -> HistoryBuffer {
        HistoryBuffer::new(&HistoryConfig {
            enabled: true,
            duration_secs: seconds,
        })
    }

    #[test]
    fn combined_audio_counts_simultaneous_sources_once() {
        let history = buffer(10);
        let now = Instant::now();
        history.push(Microphone, &vec![1; SAMPLE_RATE], now);
        history.push(Speaker, &vec![2; SAMPLE_RATE], now);
        let status = history.status(now);
        assert_eq!(status.microphone_secs, 1.0);
        assert_eq!(status.speaker_secs, 1.0);
        assert_eq!(status.combined_audio_secs, 1.0);
        assert_eq!(status.available_secs, 1.0);
        assert_eq!(status.capacity_secs, 10);
    }

    #[test]
    fn combined_audio_excludes_gaps_and_does_not_grow_when_capture_stops() {
        let history = buffer(10);
        let now = Instant::now();
        history.push(Microphone, &[1; 4800], now - Duration::from_millis(700));
        history.push(Speaker, &[2; 4800], now);
        let initial = history.status(now);
        assert_eq!(initial.combined_audio_secs, 0.6);
        assert_eq!(initial.available_secs, 1.0);
        let stopped = history.status(now + Duration::from_secs(2));
        assert_eq!(stopped.combined_audio_secs, 0.6);
        assert_eq!(stopped.available_secs, 3.0);
    }

    #[test]
    fn combined_audio_reconstructs_variable_frames_and_cross_lane_overlap() {
        let history = buffer(10);
        let now = Instant::now();
        // The later PCM chunk has an earlier nominal wall-clock start. The
        // recorder places it after the first chunk instead of overlaying it.
        history.push(Microphone, &[1; 1600], now - Duration::from_millis(500));
        history.push(
            Microphone,
            &vec![2; SAMPLE_RATE],
            now - Duration::from_millis(400),
        );
        history.push(Speaker, &[3; 8800], now);
        let status = history.status(now);
        assert_eq!(status.microphone_secs, 1.1);
        assert_eq!(status.speaker_secs, 0.55);
        assert_eq!(status.combined_audio_secs, 1.1);
    }

    #[test]
    fn combined_audio_tracks_partial_expiration_and_disabled_state() {
        let history = buffer(2);
        let now = Instant::now();
        history.push(
            Microphone,
            &vec![1; SAMPLE_RATE],
            now - Duration::from_millis(500),
        );
        history.push(Speaker, &vec![2; SAMPLE_RATE], now);
        assert_eq!(history.status(now).combined_audio_secs, 1.5);
        assert_eq!(
            history
                .status(now + Duration::from_millis(750))
                .combined_audio_secs,
            1.25
        );
        assert_eq!(
            history
                .status(now + Duration::from_secs(2))
                .combined_audio_secs,
            0.0
        );
        let fresh = now + Duration::from_secs(3);
        history.push(Microphone, &[1; 160], fresh);
        assert_eq!(history.status(fresh).combined_audio_secs, 0.01);
        history.configure(&HistoryConfig {
            enabled: false,
            duration_secs: 2,
        });
        let disabled = history.status(fresh);
        assert_eq!(disabled.combined_audio_secs, 0.0);
        assert_eq!(disabled.available_secs, 0.0);
        assert_eq!(disabled.retained_bytes, 0);
        assert_eq!(HistoryStatus::default().combined_audio_secs, 0.0);
    }

    #[test]
    fn combined_audio_preserves_all_pcm_from_a_batched_capture() {
        let history = buffer(10);
        let now = Instant::now();
        // Four distinct 10 ms PCM chunks arrive together from the audio pipe.
        for _ in 0..4 {
            history.push(Microphone, &[1; 160], now);
        }
        let status = history.status(now);
        assert_eq!(status.microphone_secs, 0.04);
        assert_eq!(status.combined_audio_secs, 0.04);
        assert_eq!(
            history
                .status(now + Duration::from_secs(1))
                .combined_audio_secs,
            0.04
        );
    }

    #[test]
    fn combined_audio_aligns_simultaneous_lanes_with_different_callback_jitter() {
        let history = buffer(10);
        let now = Instant::now();
        let origin = now - Duration::from_secs(2);
        // Both sources have 400 ms of consecutive audio. Their batching and
        // delivery jitter differ, so raw capture-time intervals would not.
        for index in 0..40_u64 {
            let end_ms = if index == 0 { 10 } else { (index / 4 + 1) * 40 };
            history.push(
                Microphone,
                &[1; 160],
                origin + Duration::from_millis(end_ms),
            );
        }
        for index in 0..20_u64 {
            let batch = index / 2;
            let end_ms = if index == 0 {
                20
            } else {
                (batch + 1) * 40 + if batch % 2 == 0 { 15 } else { 5 }
            };
            history.push(Speaker, &[2; 320], origin + Duration::from_millis(end_ms));
        }
        let status = history.status(now);
        assert_eq!(status.microphone_secs, 0.4);
        assert_eq!(status.speaker_secs, 0.4);
        assert_eq!(status.combined_audio_secs, 0.4);
        // A real 590 ms pause is excluded, unlike scheduler jitter.
        history.push(Microphone, &[3; 160], origin + Duration::from_secs(1));
        assert_eq!(history.status(now).combined_audio_secs, 0.41);
    }

    #[test]
    fn combined_batched_audio_expires_at_retained_sample_precision() {
        let history = buffer(1);
        let now = Instant::now();
        let captured = now - Duration::from_millis(990);
        for _ in 0..4 {
            history.push(Microphone, &[1; 160], captured);
        }
        assert_eq!(history.status(now).combined_audio_secs, 0.04);
        assert_eq!(
            history
                .status(now + Duration::from_millis(5))
                .combined_audio_secs,
            0.02
        );
        assert_eq!(
            history
                .status(now + Duration::from_millis(11))
                .combined_audio_secs,
            0.0
        );
    }

    #[tokio::test]
    async fn combined_audio_matches_non_silent_samples_written_by_mixed_recorder() {
        use crate::recording::{AudioRecord, SessionAudioRecorder};
        let history = buffer(10);
        let now = Instant::now();
        let origin = now - Duration::from_secs(2);
        for end_ms in [20, 40, 40, 40, 250] {
            history.push(
                Microphone,
                &[2000; 160],
                origin + Duration::from_millis(end_ms),
            );
        }
        for end_ms in [30, 50, 50, 50, 255] {
            history.push(
                Speaker,
                &[4000; 160],
                origin + Duration::from_millis(end_ms),
            );
        }
        let expected = history.status(now).combined_audio_secs;
        history.flush().await;
        assert_eq!(history.status(now).buffered_bytes, 0);
        assert!(history.status(now).encrypted_bytes > 0);
        let snapshot = history.snapshot(10, now);
        let directory = tempfile::tempdir().unwrap();
        let recorder = SessionAudioRecorder::create(
            directory.path(),
            "combined-counter",
            snapshot.origin,
            true,
            true,
        )
        .await
        .unwrap();
        let path = recorder.path().to_owned();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(sender);
        let mut reader = HistoryReader::default();
        let mut records = Vec::new();
        for frame in snapshot.frames {
            records.push(AudioRecord {
                lane: frame.lane,
                samples: frame.read_samples(&mut reader).await.unwrap().to_vec(),
                captured_at: frame.captured_at,
            });
        }
        recorder.run_with_history(receiver, records).await.unwrap();
        let reader = hound::WavReader::open(path).unwrap();
        let occupied = reader
            .into_samples::<i16>()
            .map(Result::unwrap)
            .filter(|sample| *sample != 0)
            .count();
        assert_eq!(expected, occupied as f64 / SAMPLE_RATE as f64);
        assert_eq!(expected, 0.065);
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
            history.status(now + Duration::from_secs(11)).retained_bytes,
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
            history.status(now + Duration::from_secs(11)).retained_bytes,
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
        assert_eq!(snapshot.frames[0].sample_count(), 12000);
        assert_eq!(snapshot.frames[1].sample_count(), 4000);
        assert_eq!(
            history.status(now).retained_bytes,
            SAMPLE_RATE * size_of::<i16>()
        );
    }

    #[tokio::test]
    async fn snapshots_clip_at_sample_precision_share_storage_and_preserve_lane_gaps() {
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
        assert_eq!(clipped.frames[0].sample_count(), 8000);
        assert_eq!(clipped.frames[0].captured_at, first_end);
        assert!(Arc::ptr_eq(
            &whole.frames[0].samples,
            &clipped.frames[0].samples
        ));
        assert_eq!(
            clipped.frames[1].started_at(),
            now - Duration::from_millis(410)
        );
        assert_eq!(
            clipped.frames[2]
                .read_samples(&mut HistoryReader::default())
                .await
                .unwrap()
                .as_slice(),
            &[0; 160]
        );
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
        assert_eq!(history.status(Instant::now()).retained_bytes, 0);
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
            history.status(now).retained_bytes,
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
        assert!(history.status(now).retained_bytes <= 2 * SAMPLE_RATE * size_of::<i16>());
    }

    #[tokio::test]
    async fn chronological_snapshot_merges_lanes_and_rejects_oversized_or_backwards_frames() {
        let history = buffer(10);
        let now = Instant::now();
        history.push(Speaker, &[3; 160], now);
        history.push(Microphone, &[1; 160], now - Duration::from_secs(2));
        history.push(Microphone, &[2; 160], now - Duration::from_secs(1));
        history.push(Microphone, &[99; 160], now - Duration::from_secs(3));
        history.push(Speaker, &vec![99; SAMPLE_RATE + 1], now);
        history.push(Speaker, &[], now);
        let snapshot = history.snapshot(10, now);
        let mut reader = HistoryReader::default();
        let mut values = Vec::new();
        for frame in &snapshot.frames {
            values.push(frame.read_samples(&mut reader).await.unwrap()[0]);
        }
        assert_eq!(values, [1, 2, 3]);
        assert_eq!(history.status(now).microphone_secs, 0.02);
        assert_eq!(history.status(now).speaker_secs, 0.01);
    }

    #[tokio::test]
    async fn encrypted_snapshots_keep_sample_clipping_and_survive_rolling_expiration_and_disable() {
        let directory = tempfile::tempdir().unwrap();
        let mut history = buffer(2);
        history.storage = Some(
            storage::Storage::with_limits(directory.path().to_owned(), storage::MAX_MEMORY_BYTES)
                .unwrap(),
        );
        let now = Instant::now();
        history.push(
            Microphone,
            &vec![101; SAMPLE_RATE],
            now - Duration::from_millis(1500),
        );
        history.push(Speaker, &[202; 160], now - Duration::from_millis(400));
        let clipped = history.snapshot(2, now);
        let counts = history.status(now);
        history.flush().await;
        let committed = history.status(now);
        assert_eq!(committed.buffered_bytes, 0);
        assert_eq!(committed.encrypted_bytes, committed.retained_bytes);
        assert_eq!(counts.combined_audio_secs, committed.combined_audio_secs);
        assert_eq!(counts.microphone_secs, committed.microphone_secs);
        assert_eq!(clipped.frames[0].sample_count(), 8000);
        history.prune(now + Duration::from_secs(3));
        history.configure(&HistoryConfig {
            enabled: false,
            duration_secs: 2,
        });
        assert!(
            history
                .snapshot(2, now + Duration::from_secs(3))
                .frames
                .is_empty()
        );
        let mut reader = HistoryReader::default();
        assert_eq!(
            clipped.frames[0]
                .read_samples(&mut reader)
                .await
                .unwrap()
                .as_slice(),
            &[101; 8000]
        );
        assert_eq!(
            clipped.frames[1]
                .read_samples(&mut reader)
                .await
                .unwrap()
                .as_slice(),
            &[202; 160]
        );
        assert_eq!(clipped.origin, now - Duration::from_secs(2));
        drop(clipped);
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_some()
        );
        drop(reader);
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_some()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
