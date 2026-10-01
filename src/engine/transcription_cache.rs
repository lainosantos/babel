//! Confirmed ASR results reuse the original session's encrypted journal and
//! RAM-only key. Only compact range indexes remain in memory, never whole text.
use super::*;
use crate::retention::{MAX_RECORD_BYTES, RecordId, SessionRetention};
use std::{collections::BTreeMap, io::Write};
use zeroize::Zeroizing;

const METADATA: &[u8] = b"BABEL-CONFIRMED-ASR-V1";
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Namespace {
    Live(TranscriptOrigin),
    History(TranscriptOrigin),
}
impl Namespace {
    fn index(self) -> usize {
        match self {
            Self::Live(TranscriptOrigin::Microphone) => 0,
            Self::Live(TranscriptOrigin::Speaker) => 1,
            Self::History(TranscriptOrigin::Microphone) => 2,
            Self::History(TranscriptOrigin::Speaker) => 3,
        }
    }
}
#[derive(Clone)]
struct Window {
    end: u64,
    records: Vec<RecordId>,
}
#[derive(Default)]
struct State {
    windows: [BTreeMap<u64, Window>; 4],
    records: Vec<RecordId>,
}
pub(super) struct Cache {
    store: Arc<SessionRetention>,
    state: StdMutex<State>,
}
impl Cache {
    pub(super) fn new(store: Arc<SessionRetention>) -> Self {
        Self {
            store,
            state: StdMutex::default(),
        }
    }
    pub(super) fn contains(&self, namespace: Namespace, start: u64) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).windows[namespace.index()]
            .contains_key(&start)
    }
    pub(super) async fn get(
        &self,
        namespace: Namespace,
        start: u64,
    ) -> Result<Option<(u64, Vec<TranscriptRecord>)>> {
        let window = self.state.lock().unwrap_or_else(|e| e.into_inner()).windows
            [namespace.index()]
        .get(&start)
        .cloned();
        let Some(window) = window else {
            return Ok(None);
        };
        let mut bytes = Zeroizing::new(Vec::new());
        for id in window.records {
            let mut attempts = 0u32;
            let record = loop {
                match self.store.load(id).await {
                    Ok(record) => break record,
                    Err(error) => {
                        attempts = attempts.saturating_add(1);
                        if !crate::storage::resilient::retryable_read(&error) || attempts > 3 {
                            return Err(error.context("Confirmed transcript cache could not be read; originals remain retained"));
                        }
                        tokio::time::sleep(super::resilience::delay(attempts)).await;
                    }
                }
            };
            ensure!(
                record.metadata.as_slice() == METADATA,
                "Invalid confirmed transcript cache metadata"
            );
            ensure!(
                bytes.len().saturating_add(record.bytes.len()) <= MAX_PAYLOAD,
                "Confirmed transcript cache exceeds the memory limit"
            );
            bytes.extend_from_slice(&record.bytes);
        }
        let records = if bytes.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&bytes).context("Invalid confirmed transcript cache")?
        };
        Ok(Some((window.end, records)))
    }
    pub(super) async fn put(
        &self,
        namespace: Namespace,
        start: u64,
        end: u64,
        records: &[TranscriptRecord],
    ) -> Result<()> {
        ensure!(end > start, "Confirmed transcript range must advance");
        let mut ids = Vec::new();
        if !records.is_empty() {
            let mut payload = Payload(Zeroizing::new(Vec::new()));
            serde_json::to_writer(&mut payload, records)
                .context("Confirmed transcript exceeds the cache limit")?;
            for bytes in payload.0.chunks(MAX_RECORD_BYTES) {
                let id = self
                    .store
                    .append(METADATA, bytes)
                    .await
                    .context("Could not preserve confirmed transcript results")?;
                // Also own partial cache appends until all consumers finish.
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .records
                    .push(id);
                ids.push(id);
            }
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).windows[namespace.index()]
            .insert(start, Window { end, records: ids });
        Ok(())
    }
    pub(super) async fn complete(&self) -> Result<()> {
        let ids = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .clone();
        for id in ids {
            self.store.ack(id).await?;
        }
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = State::default();
        Ok(())
    }
}
struct Payload(Zeroizing<Vec<u8>>);
impl Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_PAYLOAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Confirmed transcript cache limit reached",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn confirmed_text_is_encrypted_and_sources_and_history_remain_independent() {
        let folder = tempfile::tempdir().unwrap();
        let store = SessionRetention::create_in(folder.path(), "confirmed-text")
            .await
            .unwrap();
        let cache = Cache::new(store);
        let live = Namespace::Live(TranscriptOrigin::Microphone);
        let marker = "Private confirmed original speech: português, English, 日本語";
        cache
            .put(live, 0, 5, &[TranscriptRecord::Section(marker.into())])
            .await
            .unwrap();
        for namespace in [
            Namespace::Live(TranscriptOrigin::Speaker),
            Namespace::History(TranscriptOrigin::Microphone),
            Namespace::History(TranscriptOrigin::Speaker),
        ] {
            assert!(cache.get(namespace, 0).await.unwrap().is_none());
        }
        let (end, records) = cache.get(live, 0).await.unwrap().unwrap();
        assert_eq!(end, 5);
        assert!(matches!(&records[0], TranscriptRecord::Section(text) if text == marker));
        let journal_directory = std::fs::read_dir(folder.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let files: Vec<_> = std::fs::read_dir(&journal_directory).unwrap().collect();
        assert!(!files.is_empty());
        for file in files {
            let bytes = std::fs::read(file.unwrap().path()).unwrap();
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|part| part == marker.as_bytes())
            );
        }
        cache.complete().await.unwrap();
        assert!(cache.get(live, 0).await.unwrap().is_none());
        assert_eq!(std::fs::read_dir(journal_directory).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn silent_windows_have_no_disk_payload_and_oversized_results_do_not_commit() {
        let folder = tempfile::tempdir().unwrap();
        let store = SessionRetention::create_in(folder.path(), "confirmed-bounds")
            .await
            .unwrap();
        let cache = Cache::new(store);
        let namespace = Namespace::Live(TranscriptOrigin::Speaker);
        cache.put(namespace, 0, 1, &[]).await.unwrap();
        assert!(cache.get(namespace, 0).await.unwrap().unwrap().1.is_empty());
        assert!(
            cache
                .put(
                    namespace,
                    1,
                    2,
                    &[TranscriptRecord::Section("x".repeat(MAX_PAYLOAD))]
                )
                .await
                .is_err()
        );
        assert!(cache.get(namespace, 1).await.unwrap().is_none());
        let journal_directory = std::fs::read_dir(folder.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(std::fs::read_dir(journal_directory).unwrap().count(), 0);
    }
}
