//! Session-owned originals stream from RAM while an encrypted append journal is
//! committed in parallel. Slow readers use disk; routing never performs I/O.
use crate::{
    audio::PcmFrame,
    recording::{AudioRecord, RecordingLane},
    retention::{RecordId, SessionRetention},
    transcript::TranscriptOrigin,
};
use anyhow::{Context, Result, anyhow, ensure};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};
use tokio_util::task::AbortOnDropHandle;
use zeroize::Zeroizing;

const SAMPLE_RATE: u32 = 16_000;
const FRAME_OVERHEAD: usize = 128;
const INDEX_OVERHEAD: usize = 96;
const FRAME_HEADER: usize = 28;
const METADATA: &[u8] = b"BABEL-ORIGINAL-PCM16-V1";
const JOURNAL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
struct Limits {
    soft: usize,
    hard: usize,
    batch: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            soft: 8 * 1024 * 1024,
            hard: 64 * 1024 * 1024,
            batch: 256 * 1024,
        }
    }
}

struct Original {
    sequence: u64,
    captured_at: Instant,
    samples: Zeroizing<Vec<i16>>,
}
impl Original {
    fn memory_bytes(&self) -> usize {
        FRAME_OVERHEAD + self.samples.len() * 2
    }
}
#[derive(Clone)]
struct Batch {
    id: RecordId,
    frames: u64,
    last_sequence: u64,
}
#[derive(Default)]
struct Lane {
    pending: VecDeque<Arc<Original>>,
    stored: Vec<Batch>,
    recent: VecDeque<Arc<Original>>,
    recent_samples: usize,
    evicted_through: Option<u64>,
    last_capture: Option<Instant>,
}
#[derive(Default)]
struct State {
    lanes: [Lane; 2],
    memory_bytes: usize,
    staging_bytes: usize,
    frames: u64,
    encrypted_frames: u64,
    encrypted_bytes: u64,
    next_sequence: u64,
    force: bool,
    capture_closed: bool,
    closed: bool,
    worker_done: bool,
    completed: bool,
    error: Option<String>,
    incomplete: Option<String>,
    missing_audio: bool,
}
struct Inner {
    store: Arc<SessionRetention>,
    origin: Instant,
    limits: Limits,
    state: Mutex<State>,
    wake: Notify,
    updates: watch::Sender<u64>,
    replays: Arc<AtomicUsize>,
    stopping: tokio_util::sync::CancellationToken,
}
impl Inner {
    fn changed(&self) {
        self.updates
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct RetentionStatus {
    pub retained_frames: u64,
    pub frames: u64,
    /// Original payload stored encrypted, excluding ciphertext framing.
    pub encrypted_bytes: u64,
    pub in_memory_frames: u64,
    pub encrypted_frames: u64,
    pub memory_bytes: usize,
    pub error: Option<String>,
    pub completed: bool,
    pub capture_closed: bool,
    pub missing_audio: bool,
}

/// Keep session ownership outside provider/writer tasks. Final owner drop erases
/// the RAM-only key and removes ephemeral ciphertext through SessionRetention.
pub(super) struct RetainedSession {
    inner: Arc<Inner>,
    _worker: AbortOnDropHandle<()>,
}
impl RetainedSession {
    pub(super) fn new(
        store: Arc<SessionRetention>,
        origin: Instant,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        Self::with_limits(store, origin, runtime, Limits::default())
    }
    fn with_limits(
        store: Arc<SessionRetention>,
        origin: Instant,
        runtime: &tokio::runtime::Handle,
        limits: Limits,
    ) -> Arc<Self> {
        let (updates, _) = watch::channel(0);
        let inner = Arc::new(Inner {
            store,
            origin,
            limits,
            state: Mutex::new(State::default()),
            wake: Notify::new(),
            updates,
            replays: Arc::new(AtomicUsize::new(0)),
            stopping: tokio_util::sync::CancellationToken::new(),
        });
        Arc::new(Self {
            _worker: AbortOnDropHandle::new(runtime.spawn(spill(inner.clone()))),
            inner,
        })
    }

    /// Processing executor only: copy before recording/STT fanout. This performs
    /// no disk/network/inference work and never evicts an accepted original.
    /// An error means this new frame was NOT accepted: the caller must stop or
    /// visibly fail capture instead of reporting a complete session.
    pub(super) fn capture(&self, frame: &PcmFrame, origin: TranscriptOrigin) -> Result<u64> {
        ensure!(
            frame.sample_rate == SAMPLE_RATE
                && !frame.samples.is_empty()
                && frame.samples.len() <= SAMPLE_RATE as usize,
            "Original retention requires at most one second of 16 kHz PCM"
        );
        let lane = lane_index(origin);
        let required = FRAME_OVERHEAD + frame.samples.len() * 2;
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(
            !state.closed && !state.capture_closed,
            "Original retention capture has already closed"
        );
        ensure!(
            !state.worker_done,
            "Original retention worker is unavailable"
        );
        ensure!(
            state.lanes[lane]
                .last_capture
                .is_none_or(|previous| frame.captured_at >= previous),
            "Original retention capture clock moved backwards"
        );
        if state.staging_bytes.saturating_add(required) > self.inner.limits.hard {
            state.error = Some("Original retention memory limit reached; accepted audio remains preserved, but capture must stop until encrypted storage recovers".into());
            self.inner.wake.notify_one();
            self.inner.changed();
            return Err(anyhow!(state.error.as_ref().unwrap().clone()));
        }
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .context("Original retention sequence exhausted")?;
        let original = Arc::new(Original {
            sequence,
            captured_at: frame.captured_at,
            samples: Zeroizing::new(frame.samples.clone()),
        });
        state.lanes[lane].last_capture = Some(frame.captured_at);
        state.lanes[lane].pending.push_back(original);
        state.memory_bytes += required;
        state.staging_bytes += required;
        state.frames += 1;
        let spill = state.staging_bytes > self.inner.limits.soft;
        drop(state);
        if spill {
            self.inner.wake.notify_one();
        }
        self.inner.changed();
        Ok(sequence)
    }

    pub(super) fn origin(&self) -> Instant {
        self.inner.origin
    }
    pub(super) async fn wait_capture_closed(&self) {
        let mut changes = self.inner.updates.subscribe();
        loop {
            changes.borrow_and_update();
            if self.status().capture_closed {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }
    pub(super) fn latest_capture(&self, origin: TranscriptOrigin) -> Option<Instant> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lanes[lane_index(origin)]
        .last_capture
    }

    pub(super) fn status(&self) -> RetentionStatus {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        RetentionStatus {
            retained_frames: state.frames,
            frames: state.frames,
            encrypted_bytes: state.encrypted_bytes,
            in_memory_frames: state
                .lanes
                .iter()
                .map(|lane| lane.pending.len() as u64)
                .sum(),
            encrypted_frames: state.encrypted_frames,
            memory_bytes: state.memory_bytes,
            error: state.incomplete.clone().or_else(|| state.error.clone()),
            completed: state.completed,
            capture_closed: state.capture_closed,
            missing_audio: state.missing_audio,
        }
    }

    /// Stop accepting new originals without cancelling encrypted spill or replay.
    pub(super) fn close_capture(&self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .capture_closed = true;
        self.inner.wake.notify_one();
        self.inner.changed();
    }

    /// A failed consumer still has recoverable originals. Only an upstream gap
    /// or the bounded storage ceiling can make accepting more capture unsafe.
    pub(super) fn capture_requires_stop(&self) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.missing_audio || state.staging_bytes >= self.inner.limits.hard || state.worker_done
    }
    pub(super) fn storage_recovering(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .error
            .is_some()
    }

    /// Independent processing cursor. Slow consumers read the encrypted backlog
    /// without blocking capture, dropping frames, or pinning all PCM in RAM.
    pub(super) fn reader(self: &Arc<Self>, origin: TranscriptOrigin) -> Reader {
        self.inner.replays.fetch_add(1, Ordering::Relaxed);
        Reader {
            retained: self.clone(),
            lane: lane_index(origin),
            next_sequence: 0,
            decoded: VecDeque::new(),
            changes: self.inner.updates.subscribe(),
        }
    }

    /// Downstream failures never release originals. Keep the reason visible even
    /// if temporary encrypted-storage trouble subsequently recovers.
    pub(super) fn mark_incomplete(&self, reason: &str) {
        let reason: String = reason
            .chars()
            .filter(|character| *character != '\r' && *character != '\n')
            .take(480)
            .collect();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let existing = state.incomplete.get_or_insert_with(String::new);
        // Preserve the earliest failures: a later backlog warning must not hide
        // an earlier capture gap. Bound repeated diagnostics independently of PCM.
        if !reason.is_empty() && !existing.lines().any(|line| line == reason) {
            if existing.lines().count() < 4 {
                if !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(&reason);
            } else if !existing.ends_with("Additional processing failures occurred") {
                existing.push_str("\nAdditional processing failures occurred");
            }
        }
        drop(state);
        self.inner.changed();
    }

    /// An upstream gap cannot be repaired from the frames retained here. Keep
    /// the available originals, and never acknowledge a partial replay as whole.
    pub(super) fn mark_unrecoverable(&self, reason: &str) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .missing_audio = true;
        self.mark_incomplete(reason);
    }

    /// Freeze a prefix without interrupting capture. At most one batch per lane
    /// is decrypted during replay; originals merge by capture time, then sequence.
    pub(super) fn snapshot(&self) -> Result<Replay> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(!state.closed, "Original retention has already closed");
        self.inner.replays.fetch_add(1, Ordering::Relaxed);
        Ok(Replay {
            store: self.inner.store.clone(),
            origin: self.inner.origin,
            lanes: std::array::from_fn(|index| ReplayLane {
                batches: state.lanes[index].stored.clone().into(),
                memory: state.lanes[index].pending.clone(),
                decoded: VecDeque::new(),
            }),
            replays: self.inner.replays.clone(),
        })
    }

    /// Optional processing-side checkpoint. PCM stays in RAM if encrypted I/O
    /// fails; failure never acknowledges or deletes previously retained records.
    #[cfg(test)]
    pub(super) async fn flush(&self) -> Result<()> {
        let mut changed = self.inner.updates.subscribe();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            ensure!(!state.closed, "Original retention has already closed");
            state.force = true;
            state.error = None;
        }
        self.inner.wake.notify_one();
        loop {
            {
                let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(error) = &state.error {
                    return Err(anyhow!(error.clone()));
                }
                if state.lanes.iter().all(|lane| lane.pending.is_empty()) {
                    return Ok(());
                }
                ensure!(
                    !state.worker_done,
                    "Original retention worker ended before preserving pending audio"
                );
            }
            changed
                .changed()
                .await
                .context("Original retention worker unavailable")?;
        }
    }

    /// Call only after ALL selected outputs have committed successfully. Retry
    /// failures must retain this session instead. Active replay prevents cleanup.
    pub(super) async fn complete(&self) -> Result<()> {
        let mut changed = self.inner.updates.subscribe();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            ensure!(
                !state.missing_audio,
                "Original audio is missing; the retained partial recovery must remain available"
            );
            ensure!(
                self.inner.replays.load(Ordering::Acquire) == 0,
                "Cannot release originals while recovery is reading them"
            );
            state.capture_closed = true;
            state.closed = true;
        }
        self.inner.stopping.cancel();
        self.inner.wake.notify_one();
        loop {
            if self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .worker_done
            {
                break;
            }
            changed
                .changed()
                .await
                .context("Original retention worker unavailable")?;
        }
        let ids: Vec<_> = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .lanes
                .iter()
                .flat_map(|lane| lane.stored.iter().map(|batch| batch.id))
                .collect()
        };
        for id in ids {
            self.inner.store.ack(id).await?;
        }
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.lanes = Default::default();
        state.memory_bytes = 0;
        state.staging_bytes = 0;
        state.frames = 0;
        state.encrypted_frames = 0;
        state.encrypted_bytes = 0;
        state.error = None;
        state.incomplete = None;
        state.completed = true;
        Ok(())
    }
}

struct Finished(Arc<Inner>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker_done = true;
        self.0.changed();
    }
}
async fn spill(inner: Arc<Inner>) {
    let _finished = Finished(inner.clone());
    loop {
        let batch = {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return;
            }
            if state.lanes.iter().all(|lane| lane.pending.is_empty()) {
                state.force = false;
            }
            if !state.force && !state.capture_closed && state.staging_bytes <= inner.limits.soft {
                None
            } else {
                let lane = (0..2)
                    .filter(|&index| !state.lanes[index].pending.is_empty())
                    .min_by_key(|&index| state.lanes[index].pending.front().unwrap().captured_at);
                lane.map(|lane| {
                    let mut bytes = 4;
                    let frames = state.lanes[lane]
                        .pending
                        .iter()
                        .take_while(|frame| {
                            let next = FRAME_HEADER + frame.samples.len() * 2;
                            if bytes > 4 && bytes + next > inner.limits.batch {
                                return false;
                            }
                            bytes += next;
                            true
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    (lane, frames)
                })
            }
        };
        let Some((lane, frames)) = batch else {
            tokio::select! {
                _ = inner.wake.notified() => {},
                _ = tokio::time::sleep(JOURNAL_INTERVAL) => {
                    inner.state.lock().unwrap_or_else(|e| e.into_inner()).force = true;
                }
            }
            continue;
        };
        let result = async {
            let payload = encode(&frames, inner.origin)?;
            let mut metadata = METADATA.to_vec();
            metadata.push(lane as u8);
            inner.store.append(&metadata, &payload).await
        }
        .await;
        match result {
            Ok(id) => {
                let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                for frame in &frames {
                    let retained = state.lanes[lane].pending.pop_front().unwrap();
                    debug_assert_eq!(retained.sequence, frame.sequence);
                    state.staging_bytes -= retained.memory_bytes();
                    state.lanes[lane].recent_samples += retained.samples.len();
                    state.lanes[lane].recent.push_back(retained);
                }
                // A one-second hot cache prevents consumers close to real time
                // from racing every journal commit into a decrypt/read cycle.
                // Only already encrypted frames may leave this cache.
                while state.lanes[lane].recent_samples
                    > (SAMPLE_RATE as usize).min(inner.limits.soft / 4)
                {
                    let evicted = state.lanes[lane].recent.pop_front().unwrap();
                    state.lanes[lane].recent_samples -= evicted.samples.len();
                    state.lanes[lane].evicted_through = Some(evicted.sequence);
                    state.memory_bytes -= evicted.memory_bytes();
                }
                state.lanes[lane].stored.push(Batch {
                    id,
                    frames: frames.len() as u64,
                    last_sequence: frames.last().unwrap().sequence,
                });
                state.memory_bytes += INDEX_OVERHEAD;
                state.encrypted_frames += frames.len() as u64;
                state.encrypted_bytes += frames
                    .iter()
                    .map(|frame| frame.samples.len() as u64 * 2)
                    .sum::<u64>();
                state.error = None;
                drop(state);
                inner.changed();
            }
            Err(error) => {
                inner.state.lock().unwrap_or_else(|e| e.into_inner()).error = Some(format!(
                    "Could not preserve originals in encrypted storage; accepted audio remains in memory: {error:#}"
                ));
                inner.changed();
                tokio::select! {
                    _ = inner.stopping.cancelled() => {},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
        }
    }
}

fn lane_index(origin: TranscriptOrigin) -> usize {
    match origin {
        TranscriptOrigin::Microphone => 0,
        TranscriptOrigin::Speaker => 1,
    }
}
fn encode(frames: &[Arc<Original>], origin: Instant) -> Result<Zeroizing<Vec<u8>>> {
    let capacity = 4 + frames
        .iter()
        .map(|frame| FRAME_HEADER + frame.samples.len() * 2)
        .sum::<usize>();
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    bytes.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    for frame in frames {
        let offset = if frame.captured_at >= origin {
            i128::try_from(frame.captured_at.duration_since(origin).as_nanos())?
        } else {
            -i128::try_from(origin.duration_since(frame.captured_at).as_nanos())?
        };
        bytes.extend_from_slice(&frame.sequence.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&(frame.samples.len() as u32).to_le_bytes());
        for sample in frame.samples.iter() {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    Ok(bytes)
}
fn decode(bytes: &[u8], origin: Instant, expected: u64) -> Result<VecDeque<Arc<Original>>> {
    ensure!(bytes.len() >= 4, "Retained original batch is truncated");
    let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    ensure!(
        count as u64 == expected && count > 0 && count <= bytes.len() / (FRAME_HEADER + 2),
        "Retained original frame count is invalid"
    );
    let mut remaining = &bytes[4..];
    let mut frames = VecDeque::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        ensure!(
            remaining.len() >= FRAME_HEADER,
            "Retained original frame is truncated"
        );
        let sequence = u64::from_le_bytes(remaining[..8].try_into().unwrap());
        let offset = i128::from_le_bytes(remaining[8..24].try_into().unwrap());
        let samples = u32::from_le_bytes(remaining[24..28].try_into().unwrap()) as usize;
        remaining = &remaining[FRAME_HEADER..];
        ensure!(
            (1..=SAMPLE_RATE as usize).contains(&samples) && remaining.len() >= samples * 2,
            "Retained original sample count is invalid"
        );
        let duration = Duration::from_nanos(
            u64::try_from(offset.unsigned_abs())
                .context("Retained original timestamp is out of range")?,
        );
        let captured_at = if offset < 0 {
            origin.checked_sub(duration)
        } else {
            origin.checked_add(duration)
        }
        .context("Retained original timestamp is invalid")?;
        ensure!(
            previous.is_none_or(|time| captured_at >= time),
            "Retained original capture order is invalid"
        );
        previous = Some(captured_at);
        let pcm = remaining[..samples * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .collect();
        remaining = &remaining[samples * 2..];
        frames.push_back(Arc::new(Original {
            sequence,
            captured_at,
            samples: Zeroizing::new(pcm),
        }));
    }
    ensure!(
        remaining.is_empty(),
        "Retained original batch contains trailing data"
    );
    Ok(frames)
}
struct ReplayLane {
    batches: VecDeque<Batch>,
    memory: VecDeque<Arc<Original>>,
    decoded: VecDeque<Arc<Original>>,
}

pub(super) struct Reader {
    retained: Arc<RetainedSession>,
    lane: usize,
    next_sequence: u64,
    decoded: VecDeque<Arc<Original>>,
    changes: watch::Receiver<u64>,
}
impl Reader {
    pub(super) fn bookmark(&self) -> u64 {
        self.next_sequence
    }
    pub(super) fn rewind(&mut self, sequence: u64) {
        self.next_sequence = sequence;
        self.decoded.clear();
    }
    /// Jump to the bounded hot tail after a realtime interruption. Older
    /// originals remain in the journal for transcription and recording.
    pub(super) fn resume_recent(&mut self) -> Option<Instant> {
        let state = self
            .retained
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let lane = &state.lanes[self.lane];
        let latest = lane.pending.back().or_else(|| lane.recent.back())?;
        let cutoff = latest
            .captured_at
            .checked_sub(Duration::from_secs(1))
            .unwrap_or(latest.captured_at);
        let frame = lane
            .recent
            .iter()
            .chain(lane.pending.iter())
            .find(|frame| frame.captured_at >= cutoff)
            .unwrap_or(latest);
        // Catch-up may only advance. An exhausted or already recent reader
        // must never replay the tail indefinitely after capture EOF.
        if frame.sequence <= self.next_sequence {
            return None;
        }
        self.next_sequence = frame.sequence;
        self.decoded.clear();
        Some(frame.captured_at)
    }
    pub(super) async fn next(&mut self) -> Result<Option<AudioRecord>> {
        loop {
            if let Some(frame) = self.decoded.pop_front() {
                if frame.sequence < self.next_sequence {
                    continue;
                }
                self.next_sequence = frame.sequence + 1;
                return Ok(Some(AudioRecord {
                    lane: if self.lane == 0 {
                        RecordingLane::Microphone
                    } else {
                        RecordingLane::Speaker
                    },
                    samples: frame.samples.to_vec(),
                    captured_at: frame.captured_at,
                }));
            }
            self.changes.borrow_and_update();
            let (batch, memory, ended) = {
                let state = self
                    .retained
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                ensure!(
                    !state.closed,
                    "Original retention closed before processing completed"
                );
                let lane = &state.lanes[self.lane];
                let position = lane
                    .stored
                    .partition_point(|batch| batch.last_sequence < self.next_sequence);
                let mut batch = lane.stored.get(position).cloned();
                let recent = if lane
                    .evicted_through
                    .is_none_or(|sequence| sequence < self.next_sequence)
                {
                    let position = lane
                        .recent
                        .partition_point(|frame| frame.sequence < self.next_sequence);
                    lane.recent.get(position).cloned()
                } else {
                    None
                };
                let position = lane
                    .pending
                    .partition_point(|frame| frame.sequence < self.next_sequence);
                let memory = recent.or_else(|| {
                    if batch.is_none() {
                        lane.pending.get(position).cloned()
                    } else {
                        None
                    }
                });
                if memory.is_some() {
                    batch = None;
                }
                let uncommitted = lane
                    .pending
                    .back()
                    .is_some_and(|frame| frame.sequence >= self.next_sequence);
                (batch, memory, state.capture_closed && !uncommitted)
            };
            if let Some(batch) = batch {
                let mut attempts = 0u32;
                let data = loop {
                    match self.retained.inner.store.load(batch.id).await {
                        Ok(data) => break data,
                        Err(_) => {
                            attempts = attempts.saturating_add(1);
                            if attempts == 1 {
                                tracing::warn!(
                                    "Retained original read failed; retrying the same batch"
                                );
                            }
                            tokio::time::sleep(super::resilience::delay(attempts)).await;
                        }
                    }
                };
                ensure!(
                    data.metadata.len() == METADATA.len() + 1
                        && data.metadata[..METADATA.len()] == *METADATA
                        && data.metadata[METADATA.len()] == self.lane as u8,
                    "Retained original metadata is invalid"
                );
                self.decoded = decode(&data.bytes, self.retained.inner.origin, batch.frames)?;
            } else if let Some(frame) = memory {
                self.decoded.push_back(frame);
            } else if ended {
                return Ok(None);
            } else {
                self.changes
                    .changed()
                    .await
                    .context("Original retention updates closed")?;
            }
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.retained.inner.replays.fetch_sub(1, Ordering::Release);
    }
}
pub(super) struct Replay {
    store: Arc<SessionRetention>,
    origin: Instant,
    lanes: [ReplayLane; 2],
    replays: Arc<AtomicUsize>,
}
impl Replay {
    pub(super) async fn next(&mut self) -> Result<Option<AudioRecord>> {
        for index in 0..2 {
            let lane = &mut self.lanes[index];
            if lane.decoded.is_empty()
                && let Some(batch) = lane.batches.front()
            {
                let retained = self.store.load(batch.id).await?;
                ensure!(
                    retained.metadata.len() == METADATA.len() + 1
                        && retained.metadata[..METADATA.len()] == *METADATA
                        && retained.metadata[METADATA.len()] == index as u8,
                    "Retained original metadata is invalid"
                );
                lane.decoded = decode(&retained.bytes, self.origin, batch.frames)?;
                lane.batches.pop_front();
            }
        }
        let lane = (0..2)
            .filter_map(|index| {
                self.lanes[index]
                    .decoded
                    .front()
                    .or_else(|| self.lanes[index].memory.front())
                    .map(|frame| (index, frame.captured_at, frame.sequence))
            })
            .min_by_key(|(_, at, sequence)| (*at, *sequence))
            .map(|(index, _, _)| index);
        let Some(index) = lane else {
            return Ok(None);
        };
        let lane = &mut self.lanes[index];
        let frame = lane
            .decoded
            .pop_front()
            .or_else(|| lane.memory.pop_front())
            .unwrap();
        Ok(Some(AudioRecord {
            lane: if index == 0 {
                RecordingLane::Microphone
            } else {
                RecordingLane::Speaker
            },
            captured_at: frame.captured_at,
            samples: frame.samples.to_vec(),
        }))
    }
}
impl Drop for Replay {
    fn drop(&mut self) {
        self.replays.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn session(limits: Limits) -> (tempfile::TempDir, Arc<RetainedSession>, Instant) {
        let folder = tempfile::tempdir().unwrap();
        let store = SessionRetention::create_in(folder.path(), "retained-fixture")
            .await
            .unwrap();
        let origin = Instant::now() - Duration::from_secs(1);
        let retained =
            RetainedSession::with_limits(store, origin, &tokio::runtime::Handle::current(), limits);
        (folder, retained, origin)
    }
    fn frame(value: i16, captured_at: Instant) -> PcmFrame {
        PcmFrame {
            samples: vec![value; 16],
            sample_rate: SAMPLE_RATE,
            captured_at,
        }
    }
    async fn collect(mut replay: Replay) -> Vec<AudioRecord> {
        let mut records = Vec::new();
        while let Some(frame) = replay.next().await.unwrap() {
            // Only two small batches are decrypted, not the full session.
            assert!(replay.lanes.iter().all(|lane| lane.decoded.len() <= 64));
            records.push(frame);
        }
        records
    }

    #[tokio::test]
    async fn translation_can_rewind_or_jump_recent_without_discarding_originals() {
        let (_directory, retained, origin) = session(Limits::default()).await;
        for index in 0..60_u64 {
            retained
                .capture(
                    &frame(index as i16, origin + Duration::from_millis(index * 100)),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
        }
        retained.close_capture();
        let mut reader = retained.reader(TranscriptOrigin::Microphone);
        assert_eq!(reader.next().await.unwrap().unwrap().samples, vec![0; 16]);
        let checkpoint = reader.bookmark();
        let second = reader.next().await.unwrap().unwrap();
        reader.rewind(checkpoint);
        assert_eq!(
            reader.next().await.unwrap().unwrap().samples,
            second.samples
        );
        let recent = reader.resume_recent().unwrap();
        assert!(recent >= origin + Duration::from_millis(4900));
        assert_eq!(reader.next().await.unwrap().unwrap().captured_at, recent);
        assert!(reader.resume_recent().is_none());
        while reader.next().await.unwrap().is_some() {}
        assert!(reader.resume_recent().is_none());
        assert!(reader.next().await.unwrap().is_none());
        let all = collect(retained.snapshot().unwrap()).await;
        assert_eq!(all.len(), 60);
        assert_eq!(all[0].samples, vec![0; 16]);
    }

    #[tokio::test]
    async fn independent_slow_readers_finish_every_frame_across_encrypted_spill_and_eof() {
        let (_directory, retained, origin) = session(Limits {
            soft: 1024,
            hard: 64 * 1024,
            batch: 256,
        })
        .await;
        let mut live = retained.reader(TranscriptOrigin::Microphone);
        let mut delayed = retained.reader(TranscriptOrigin::Microphone);
        let mut output = retained.reader(TranscriptOrigin::Speaker);
        for index in 0..80 {
            retained
                .capture(
                    &frame(index, origin + Duration::from_millis(index as u64)),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
            retained
                .capture(
                    &frame(-index, origin + Duration::from_millis(index as u64)),
                    TranscriptOrigin::Speaker,
                )
                .unwrap();
            assert_eq!(live.next().await.unwrap().unwrap().samples, vec![index; 16]);
        }
        retained.close_capture();
        retained.flush().await.unwrap();
        assert!(retained.status().encrypted_frames > 0);
        for index in 0..80 {
            assert_eq!(
                delayed.next().await.unwrap().unwrap().samples,
                vec![index; 16]
            );
            assert_eq!(
                output.next().await.unwrap().unwrap().samples,
                vec![-index; 16]
            );
        }
        assert!(live.next().await.unwrap().is_none());
        assert!(delayed.next().await.unwrap().is_none());
        assert!(output.next().await.unwrap().is_none());
        assert!(
            retained.complete().await.is_err(),
            "Reader ownership prevents premature key erasure"
        );
        drop((live, delayed, output));
        retained.complete().await.unwrap();
    }

    #[tokio::test]
    async fn recent_original_is_available_before_any_journal_commit() {
        let (_directory, retained, origin) = session(Limits::default()).await;
        let mut reader = retained.reader(TranscriptOrigin::Microphone);
        retained
            .capture(&frame(321, origin), TranscriptOrigin::Microphone)
            .unwrap();
        assert_eq!(retained.status().encrypted_frames, 0);
        let received = tokio::time::timeout(Duration::ZERO, reader.next())
            .await
            .expect("Recent inference input never waits for disk")
            .unwrap()
            .unwrap();
        assert_eq!(received.samples, vec![321; 16]);
        assert_eq!(retained.status().encrypted_frames, 0);
    }

    #[tokio::test]
    async fn encrypted_batches_replay_both_original_lanes_in_capture_order() {
        let (_folder, retained, origin) = session(Limits {
            soft: 1024,
            hard: 64 * 1024,
            batch: 256,
        })
        .await;
        for index in 0..80_u64 {
            // Include historical audio before the session reference clock.
            let at = origin - Duration::from_millis(20) + Duration::from_millis(index);
            let lane = if index % 2 == 0 {
                TranscriptOrigin::Microphone
            } else {
                TranscriptOrigin::Speaker
            };
            retained.capture(&frame(index as i16, at), lane).unwrap();
        }
        retained.close_capture();
        tokio::time::timeout(Duration::from_secs(5), retained.flush())
            .await
            .unwrap()
            .unwrap();
        let status = retained.status();
        assert_eq!(status.frames, 80);
        assert_eq!(status.in_memory_frames, 0);
        assert_eq!(status.encrypted_frames, 80);
        assert_eq!(status.encrypted_bytes, 80 * 16 * 2);
        assert!(status.memory_bytes < 80 * (FRAME_OVERHEAD + 32));
        let records = collect(retained.snapshot().unwrap()).await;
        assert_eq!(records.len(), 80);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.samples, vec![index as i16; 16]);
            assert_eq!(
                record.captured_at,
                origin - Duration::from_millis(20) + Duration::from_millis(index as u64)
            );
            assert_eq!(
                record.lane,
                if index % 2 == 0 {
                    RecordingLane::Microphone
                } else {
                    RecordingLane::Speaker
                }
            );
        }
        assert_eq!(
            retained.status().frames,
            80,
            "Replay never acknowledges originals"
        );
    }

    #[tokio::test]
    async fn snapshot_preserves_a_prefix_while_spilling_and_capture_continue() {
        let (_folder, retained, origin) = session(Limits {
            soft: 512,
            hard: 64 * 1024,
            batch: 256,
        })
        .await;
        for index in 0..20 {
            retained
                .capture(
                    &frame(index, origin + Duration::from_millis(index as u64)),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
        }
        let snapshot = retained.snapshot().unwrap();
        for index in 20..40 {
            retained
                .capture(
                    &frame(index, origin + Duration::from_millis(index as u64)),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
        }
        retained.flush().await.unwrap();
        let prefix = collect(snapshot).await;
        assert_eq!(prefix.len(), 20);
        for (index, record) in prefix.iter().enumerate() {
            assert_eq!(record.samples, vec![index as i16; 16]);
        }
        let complete = collect(retained.snapshot().unwrap()).await;
        assert_eq!(complete.len(), 40);
        for (index, record) in complete.iter().enumerate() {
            assert_eq!(record.samples, vec![index as i16; 16]);
        }
    }

    #[tokio::test]
    async fn memory_limit_rejects_new_input_without_overwriting_accepted_originals() {
        let (_folder, retained, origin) = session(Limits {
            soft: 640,
            hard: 640,
            batch: 256,
        })
        .await;
        for index in 0..4 {
            retained
                .capture(
                    &frame(index, origin + Duration::from_millis(index as u64)),
                    TranscriptOrigin::Microphone,
                )
                .unwrap();
        }
        assert!(
            retained
                .capture(
                    &frame(99, origin + Duration::from_millis(5)),
                    TranscriptOrigin::Microphone
                )
                .is_err()
        );
        assert_eq!(retained.status().memory_bytes, 640);
        assert_eq!(retained.status().frames, 4);
        assert!(retained.status().error.unwrap().contains("memory limit"));
        assert!(retained.capture_requires_stop());
        retained.close_capture();
        assert!(
            retained
                .capture(
                    &frame(100, origin + Duration::from_millis(6)),
                    TranscriptOrigin::Microphone
                )
                .is_err()
        );
        let saved = collect(retained.snapshot().unwrap()).await;
        assert_eq!(
            saved
                .iter()
                .map(|frame| frame.samples[0])
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[tokio::test]
    async fn unavailable_encrypted_storage_keeps_originals_in_ram_for_recovery() {
        let (folder, retained, origin) = session(Limits {
            soft: 1024,
            hard: 4096,
            batch: 256,
        })
        .await;
        retained
            .capture(&frame(1234, origin), TranscriptOrigin::Speaker)
            .unwrap();
        for entry in std::fs::read_dir(folder.path()).unwrap() {
            std::fs::remove_dir_all(entry.unwrap().path()).unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(5), retained.flush())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(retained.status().in_memory_frames, 1);
        assert_eq!(retained.status().encrypted_frames, 0);
        assert!(retained.storage_recovering());
        assert!(
            !retained.capture_requires_stop(),
            "A temporary storage outage must retry while staging has capacity"
        );
        let records = collect(retained.snapshot().unwrap()).await;
        assert_eq!(records[0].samples, vec![1234; 16]);
        assert_eq!(records[0].lane, RecordingLane::Speaker);
    }

    #[tokio::test]
    async fn downstream_failure_preserves_replay_until_explicit_successful_completion() {
        let (folder, retained, origin) = session(Limits {
            soft: 1024,
            hard: 4096,
            batch: 256,
        })
        .await;
        retained
            .capture(&frame(4321, origin), TranscriptOrigin::Microphone)
            .unwrap();
        retained.mark_incomplete("Transcript output failed");
        retained.mark_incomplete("Recognizer disconnected; retry pending");
        retained.mark_incomplete("Recognizer disconnected; retry pending");
        assert!(retained.status().error.is_some());
        assert!(
            !retained.capture_requires_stop(),
            "A failed transcript must not stop capture or independent translation"
        );
        retained
            .capture(
                &frame(5678, origin + Duration::from_millis(1)),
                TranscriptOrigin::Speaker,
            )
            .unwrap();
        retained.flush().await.unwrap();
        let reasons = retained.status().error.unwrap();
        assert!(reasons.contains("Transcript output failed"));
        assert!(reasons.contains("retry pending"));
        assert_eq!(reasons.lines().count(), 2);
        let snapshot = retained.snapshot().unwrap();
        assert!(
            retained.complete().await.is_err(),
            "Do not delete ciphertext under an active reader"
        );
        let originals = collect(snapshot).await;
        assert_eq!(originals.len(), 2);
        retained.complete().await.unwrap();
        assert_eq!(retained.status().frames, 0);
        assert_eq!(retained.status().memory_bytes, 0);
        assert!(retained.status().completed);
        assert!(retained.snapshot().is_err());
        for entry in std::fs::read_dir(folder.path()).unwrap() {
            assert_eq!(std::fs::read_dir(entry.unwrap().path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn an_upstream_gap_cannot_be_acknowledged_as_complete_after_replay() {
        let (_folder, retained, origin) = session(Limits::default()).await;
        retained
            .capture(&frame(44, origin), TranscriptOrigin::Speaker)
            .unwrap();
        retained.mark_unrecoverable("Capture queue discarded an original frame");
        retained.mark_incomplete("Transcript output still needs recovery");
        let available = collect(retained.snapshot().unwrap()).await;
        assert_eq!(available.len(), 1);
        assert!(retained.status().missing_audio);
        assert!(retained.capture_requires_stop());
        assert!(retained.complete().await.is_err());
        assert_eq!(retained.status().frames, 1);
        assert!(!retained.status().completed);
        assert_eq!(
            collect(retained.snapshot().unwrap()).await[0].samples,
            vec![44; 16]
        );
    }

    #[tokio::test]
    async fn malformed_or_backward_frames_cannot_corrupt_the_retained_timeline() {
        let (_folder, retained, origin) = session(Limits::default()).await;
        retained
            .capture(&frame(7, origin), TranscriptOrigin::Microphone)
            .unwrap();
        assert!(
            retained
                .capture(
                    &frame(8, origin - Duration::from_millis(1)),
                    TranscriptOrigin::Microphone
                )
                .is_err()
        );
        let mut invalid = frame(9, origin);
        invalid.sample_rate = 48_000;
        assert!(
            retained
                .capture(&invalid, TranscriptOrigin::Speaker)
                .is_err()
        );
        assert_eq!(retained.status().frames, 1);
        assert_eq!(
            collect(retained.snapshot().unwrap()).await[0].samples,
            vec![7; 16]
        );
    }
}
