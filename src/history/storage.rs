//! Bounded plaintext staging and rotating encrypted history journals. The
//! retention executor owns encryption, disk I/O and temporary-folder cleanup.
use crate::retention::{RecordId, SessionRetention};
use anyhow::{Context, Result, ensure};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::Notify};
use tokio_util::task::AbortOnDropHandle;
use zeroize::Zeroizing;

const METADATA: &[u8] = b"BABEL-HISTORY-PCM16-V1";
const BATCH_BYTES: usize = 256 * 1024;
const SEGMENT_BYTES: usize = 1024 * 1024;
const SEGMENT_DURATION: Duration = Duration::from_secs(5);
const COMMIT_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PENDING_FRAMES: usize = 8192;
pub(super) const MAX_MEMORY_BYTES: usize = 8 * 1024 * 1024;

struct Store {
    retention: Option<Arc<SessionRetention>>,
    runtime: Handle,
}
impl Store {
    fn retention(&self) -> &Arc<SessionRetention> {
        self.retention.as_ref().unwrap()
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        // A rolling prune may run on the control plane. TempDir removal must
        // stay on the retention pool, including the last snapshot's release.
        let retention = self.retention.take();
        self.runtime.spawn_blocking(move || drop(retention));
    }
}

struct Batch {
    store: Arc<Store>,
    id: RecordId,
    bytes: usize,
}
enum Location {
    Memory(Zeroizing<Vec<i16>>),
    Encrypted { batch: Arc<Batch>, offset: usize },
}
pub struct Audio {
    location: Mutex<Location>,
    samples: usize,
    memory: Arc<AtomicUsize>,
}
impl std::fmt::Debug for Audio {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryAudio")
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}
impl Audio {
    pub(super) fn memory_bytes(&self) -> usize {
        match &*self.location.lock().unwrap_or_else(|e| e.into_inner()) {
            Location::Memory(_) => self.samples * 2,
            Location::Encrypted { .. } => 0,
        }
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        if matches!(
            self.location.get_mut().unwrap_or_else(|e| e.into_inner()),
            Location::Memory(_)
        ) {
            self.memory.fetch_sub(self.samples * 2, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Weak<Audio>>,
    inflight: bool,
    error: Option<String>,
    dropped_frames: u64,
    last_drop: Option<Instant>,
}
struct Inner {
    queue: Mutex<Queue>,
    memory: Arc<AtomicUsize>,
    wake: Notify,
    base: PathBuf,
    runtime: Handle,
    max_memory: usize,
}
pub(super) struct Storage {
    inner: Arc<Inner>,
    _worker: AbortOnDropHandle<()>,
}
impl Storage {
    pub(super) fn new() -> Result<Self> {
        Self::with_limits(std::env::temp_dir(), MAX_MEMORY_BYTES)
    }
    pub(super) fn with_limits(base: PathBuf, max_memory: usize) -> Result<Self> {
        let runtime = crate::execution::retention_handle()?;
        let inner = Arc::new(Inner {
            queue: Mutex::new(Queue::default()),
            memory: Arc::new(AtomicUsize::new(0)),
            wake: Notify::new(),
            base,
            runtime: runtime.clone(),
            max_memory,
        });
        let worker = AbortOnDropHandle::new(runtime.spawn(commit(inner.clone())));
        Ok(Self {
            inner,
            _worker: worker,
        })
    }
    pub(super) fn stage(&self, samples: &[i16]) -> Option<Arc<Audio>> {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.pending.retain(|audio| audio.strong_count() > 0);
        if queue.pending.len() >= MAX_PENDING_FRAMES
            || self.inner.memory.load(Ordering::Relaxed) + samples.len() * 2 > self.inner.max_memory
        {
            queue.dropped_frames += 1;
            queue.last_drop = Some(Instant::now());
            queue.error = Some("Recent audio capture has gaps: encrypted storage could not keep up with the bounded memory queue".into());
            return None;
        }
        self.inner
            .memory
            .fetch_add(samples.len() * 2, Ordering::Relaxed);
        let audio = Arc::new(Audio {
            location: Mutex::new(Location::Memory(Zeroizing::new(samples.to_vec()))),
            samples: samples.len(),
            memory: self.inner.memory.clone(),
        });
        queue.pending.push_back(Arc::downgrade(&audio));
        if self.inner.memory.load(Ordering::Relaxed) >= BATCH_BYTES {
            self.inner.wake.notify_one();
        }
        Some(audio)
    }
    pub(super) fn expire_gaps(&self, cutoff: Instant) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.last_drop.is_some_and(|at| at < cutoff) {
            queue.last_drop = None;
            queue.dropped_frames = 0;
        }
    }

    pub(super) fn reset_gaps(&self) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.last_drop = None;
        queue.dropped_frames = 0;
    }

    pub(super) fn status(&self) -> (usize, Option<String>, u64) {
        let queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        (
            self.inner.memory.load(Ordering::Relaxed),
            queue.error.clone(),
            queue.dropped_frames,
        )
    }
    #[cfg(test)]
    pub(super) async fn flush(&self) {
        loop {
            let done = {
                let mut queue = self.inner.queue.lock().unwrap();
                queue.pending.retain(|audio| audio.strong_count() > 0);
                queue.pending.is_empty() && !queue.inflight
            };
            if done {
                return;
            }
            self.inner.wake.notify_one();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn commit(inner: Arc<Inner>) {
    let mut segment: Option<(Weak<Store>, Instant, usize)> = None;
    let mut failures = 0u32;
    let mut retry_at = None;
    loop {
        tokio::select! { _ = inner.wake.notified() => {}, _ = tokio::time::sleep(COMMIT_INTERVAL) => {} }
        if let Some(deadline) = retry_at.take() {
            tokio::time::sleep_until(deadline).await;
        }
        let frames = {
            let mut queue = inner.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue.pending.retain(|audio| audio.strong_count() > 0);
            let mut bytes = 0;
            let frames = queue
                .pending
                .iter()
                .filter_map(Weak::upgrade)
                .take_while(|audio| {
                    bytes += audio.samples * 2;
                    bytes <= BATCH_BYTES
                })
                .collect::<Vec<_>>();
            queue.inflight = !frames.is_empty();
            frames
        };
        if frames.is_empty() {
            if segment
                .as_ref()
                .is_some_and(|(_, started, _)| started.elapsed() >= SEGMENT_DURATION)
            {
                segment = None;
            }
            continue;
        }
        let result = async {
            let previous = segment.as_ref().and_then(|(store, _, _)| store.upgrade());
            let reusable = previous.filter(|_| {
                segment.as_ref().is_some_and(|(_, started, bytes)| {
                    started.elapsed() < SEGMENT_DURATION
                        && *bytes + frames.iter().map(|audio| audio.samples * 2).sum::<usize>()
                            <= SEGMENT_BYTES
                })
            });
            let store = if let Some(store) = reusable {
                store
            } else {
                let store = Arc::new(Store {
                    retention: Some(
                        SessionRetention::create_in(&inner.base, "recent-history").await?,
                    ),
                    runtime: inner.runtime.clone(),
                });
                segment = Some((Arc::downgrade(&store), Instant::now(), 0));
                store
            };
            let mut bytes = Zeroizing::new(Vec::new());
            let mut offsets = Vec::with_capacity(frames.len());
            for audio in &frames {
                offsets.push(bytes.len());
                let location = audio.location.lock().unwrap_or_else(|e| e.into_inner());
                let Location::Memory(samples) = &*location else {
                    anyhow::bail!("History staging record was already committed");
                };
                for sample in samples.iter() {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
            }
            let (_, _, total) = segment.as_mut().unwrap();
            let id = store.retention().append(METADATA, &bytes).await?;
            *total += bytes.len();
            let batch = Arc::new(Batch {
                store: store.clone(),
                id,
                bytes: bytes.len(),
            });
            for (audio, offset) in frames.iter().zip(offsets) {
                *audio.location.lock().unwrap_or_else(|e| e.into_inner()) = Location::Encrypted {
                    batch: batch.clone(),
                    offset,
                };
                inner.memory.fetch_sub(audio.samples * 2, Ordering::Relaxed);
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let mut queue = inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.inflight = false;
        match result {
            Ok(()) => {
                failures = 0;
                queue.pending.retain(|audio| {
                    audio
                        .upgrade()
                        .is_some_and(|audio| audio.memory_bytes() > 0)
                });
                queue.error = None;
                if !queue.pending.is_empty() {
                    inner.wake.notify_one();
                }
            }
            Err(_) => {
                queue.error = Some("Recent audio storage is recovering; accepted audio remains in the bounded memory queue".into());
                // Retry the same originals, without advancing their cursor or
                // acknowledging any failed encrypted append.
                let delay = Duration::from_millis((250u64 << failures.min(5)).min(5000));
                failures = failures.saturating_add(1);
                retry_at = Some(tokio::time::Instant::now() + delay);
            }
        }
    }
}

/// Each consumer decrypts at most two small batches, never the entire prefix.
#[derive(Default)]
pub struct Reader {
    batches: VecDeque<(Arc<Batch>, Zeroizing<Vec<u8>>)>,
}
impl Reader {
    pub(super) async fn samples(
        &mut self,
        audio: &Audio,
        range: std::ops::Range<usize>,
    ) -> Result<Zeroizing<Vec<i16>>> {
        let (batch, offset) = {
            let location = audio.location.lock().unwrap_or_else(|e| e.into_inner());
            match &*location {
                Location::Memory(samples) => return Ok(Zeroizing::new(samples[range].to_vec())),
                Location::Encrypted { batch, offset } => (batch.clone(), *offset),
            }
        };
        if !self
            .batches
            .iter()
            .any(|(cached, _)| Arc::ptr_eq(cached, &batch))
        {
            let mut attempts = 0u32;
            let record = loop {
                match batch.store.retention().load(batch.id).await {
                    Ok(record) => break record,
                    Err(error)
                        if error.chain().any(|cause| {
                            cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
                                !matches!(
                                    io.kind(),
                                    std::io::ErrorKind::InvalidInput
                                        | std::io::ErrorKind::InvalidData
                                )
                            })
                        }) =>
                    {
                        if attempts == 0 {
                            tracing::warn!(
                                "Recent history read interrupted; retrying the same encrypted batch"
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(
                            (250u64 << attempts.min(5)).min(5000),
                        ))
                        .await;
                        attempts = attempts.saturating_add(1);
                    }
                    Err(error) => {
                        return Err(error).context("Could not recover encrypted recent history");
                    }
                }
            };
            ensure!(
                record.metadata.as_slice() == METADATA && record.bytes.len() == batch.bytes,
                "Invalid encrypted history batch"
            );
            if self.batches.len() == 2 {
                self.batches.pop_front();
            }
            self.batches.push_back((batch.clone(), record.bytes));
        }
        let (_, bytes) = self
            .batches
            .iter()
            .find(|(cached, _)| Arc::ptr_eq(cached, &batch))
            .unwrap();
        let start = offset + range.start * 2;
        let end = offset + range.end * 2;
        ensure!(end <= bytes.len(), "Invalid encrypted history sample range");
        Ok(Zeroizing::new(
            bytes[start..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn flush(storage: &Storage) {
        tokio::time::timeout(Duration::from_secs(5), storage.flush())
            .await
            .unwrap();
    }

    async fn empty_folder(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_dir(path).unwrap().next().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn committed_pcm_is_encrypted_and_snapshots_pin_files_until_their_last_owner() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::with_limits(directory.path().to_owned(), MAX_MEMORY_BYTES).unwrap();
        let first = storage.stage(&[0x2345; 1600]).unwrap();
        let second = storage.stage(&[-1234; 320]).unwrap();
        flush(&storage).await;
        assert_eq!(storage.status().0, 0);
        let folders = std::fs::read_dir(directory.path())
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(folders.len(), 1);
        let files = std::fs::read_dir(folders[0].path())
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(files.len(), 1, "Only the encrypted journal is persisted");
        let ciphertext = std::fs::read(files[0].path()).unwrap();
        assert!(
            !ciphertext
                .windows(16)
                .any(|bytes| bytes == [0x45, 0x23].repeat(8))
        );
        assert!(
            !ciphertext
                .windows(METADATA.len())
                .any(|bytes| bytes == METADATA)
        );
        drop(storage);
        let mut reader = Reader::default();
        assert_eq!(
            reader.samples(&first, 400..1600).await.unwrap().as_slice(),
            &[0x2345; 1200]
        );
        assert_eq!(
            reader.samples(&second, 0..320).await.unwrap().as_slice(),
            &[-1234; 320]
        );
        drop(first);
        drop(second);
        assert!(
            files[0].path().exists(),
            "An active decrypted reader still owns the key and journal"
        );
        drop(reader);
        empty_folder(directory.path()).await;
    }

    #[tokio::test]
    async fn disk_failure_preserves_the_accepted_queue_and_recovers_without_repeating_pcm() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("blocked");
        std::fs::write(&base, b"unavailable folder").unwrap();
        let storage = Storage::with_limits(base.clone(), 640).unwrap();
        let first = storage.stage(&[11; 160]).unwrap();
        let second = storage.stage(&[22; 160]).unwrap();
        storage.inner.wake.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while storage.status().1.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(storage.status().0, 640);
        assert!(storage.stage(&[33; 160]).is_none());
        assert_eq!(storage.status().2, 1);
        let mut memory_reader = Reader::default();
        assert_eq!(
            memory_reader
                .samples(&first, 0..160)
                .await
                .unwrap()
                .as_slice(),
            &[11; 160]
        );
        std::fs::remove_file(&base).unwrap();
        std::fs::create_dir(&base).unwrap();
        flush(&storage).await;
        assert_eq!(storage.status().0, 0);
        assert!(storage.status().1.is_none());
        let third = storage.stage(&[44; 160]).unwrap();
        flush(&storage).await;
        let mut reader = Reader::default();
        for (audio, value) in [(&first, 11), (&second, 22), (&third, 44)] {
            assert_eq!(
                reader.samples(audio, 0..160).await.unwrap().as_slice(),
                &[value; 160]
            );
        }
        drop((first, second, third, reader, storage));
        empty_folder(&base).await;
    }

    #[tokio::test]
    async fn replay_cache_is_bounded_and_corrupted_ciphertext_cannot_become_audio() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::with_limits(directory.path().to_owned(), MAX_MEMORY_BYTES).unwrap();
        let mut frames = Vec::new();
        for value in 0..4 {
            frames.push(storage.stage(&vec![value; 16000]).unwrap());
            flush(&storage).await;
        }
        let mut reader = Reader::default();
        for (value, frame) in frames.iter().enumerate() {
            assert_eq!(
                reader.samples(frame, 0..16000).await.unwrap()[0],
                value as i16
            );
            assert!(reader.batches.len() <= 2);
            assert!(
                reader
                    .batches
                    .iter()
                    .map(|(_, bytes)| bytes.len())
                    .sum::<usize>()
                    <= 2 * BATCH_BYTES
            );
        }
        let folder = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = folder.join("session.brj");
        let mut ciphertext = std::fs::read(&path).unwrap();
        ciphertext[80] ^= 0x80;
        std::fs::write(&path, ciphertext).unwrap();
        let mut uncached = Reader::default();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                uncached.samples(&frames[0], 0..16000)
            )
            .await
            .unwrap()
            .is_err()
        );
        drop((frames, storage, reader, uncached));
        empty_folder(directory.path()).await;
    }

    #[tokio::test]
    async fn rotating_journals_reclaim_expired_segments_and_transient_reads_resume() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::with_limits(directory.path().to_owned(), MAX_MEMORY_BYTES).unwrap();
        let mut frames = Vec::new();
        for value in 0..40 {
            frames.push(storage.stage(&vec![value; 16000]).unwrap());
            if frames.len() % 8 == 0 {
                flush(&storage).await;
            }
        }
        assert_eq!(storage.status().0, 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        // The first four 256 KiB batches belong to one segment. Releasing all
        // its originals reclaims its journal while the newer segment stays live.
        drop(frames.drain(..32));
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_dir(directory.path()).unwrap().count() != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let folder = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let journal = folder.join("session.brj");
        let unavailable = folder.join("temporarily-unavailable");
        std::fs::rename(&journal, &unavailable).unwrap();
        let frame = frames[0].clone();
        let read = tokio::spawn(async move { Reader::default().samples(&frame, 0..16000).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !read.is_finished(),
            "A temporary read failure keeps the same original pending"
        );
        std::fs::rename(unavailable, journal).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .as_slice(),
            &[32; 16000]
        );
        drop((frames, storage));
        empty_folder(directory.path()).await;
    }
}
