//! Idempotent buffered file writes. Retry the same bytes at the confirmed offset
//! after a partial write; never advance a recording/text cursor on an I/O error.
use std::{io, sync::Arc, time::Duration};
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
};

const CAPACITY: usize = 64 * 1024;
pub(crate) type RecoveryObserver = Arc<dyn Fn(bool) + Send + Sync>;

pub(crate) struct ResilientFile {
    file: File,
    offset: u64,
    pending: Vec<u8>,
    observer: Option<RecoveryObserver>,
    #[cfg(test)]
    partial_failures: usize,
}

impl ResilientFile {
    pub(crate) fn new(file: File) -> Self {
        Self {
            file,
            offset: 0,
            pending: Vec::with_capacity(CAPACITY),
            observer: None,
            #[cfg(test)]
            partial_failures: 0,
        }
    }

    pub(crate) fn observe(&mut self, observer: RecoveryObserver) {
        self.observer = Some(observer);
    }

    fn recovering(&self, active: bool) {
        if let Some(observer) = &self.observer {
            observer(active);
        }
    }

    pub(crate) async fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = bytes.len().min(CAPACITY - self.pending.len());
            self.pending.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.pending.len() == CAPACITY {
                self.flush().await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let end = self
            .offset
            .checked_add(self.pending.len() as u64)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Session file offset overflow")
            })?;
        let mut attempts = 0;
        loop {
            let result = async {
                self.file.seek(io::SeekFrom::Start(self.offset)).await?;
                #[cfg(test)]
                if self.partial_failures > 0 {
                    self.partial_failures -= 1;
                    let count = self.pending.len().div_ceil(2);
                    self.file.write_all(&self.pending[..count]).await?;
                    return Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "Synthetic partial write",
                    ));
                }
                self.file.write_all(&self.pending).await?;
                self.file.flush().await
            }
            .await;
            match result {
                Ok(()) => {
                    self.offset = end;
                    self.pending.clear();
                    self.recovering(false);
                    return Ok(());
                }
                Err(error) => self.retry(error, &mut attempts).await?,
            }
        }
    }

    pub(crate) async fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.flush().await?;
        // Callers use absolute offsets for the WAV header and confirmed tail.
        let io::SeekFrom::Start(offset) = position else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Session files require absolute seeks",
            ));
        };
        self.offset = offset;
        Ok(offset)
    }

    pub(crate) async fn sync_data(&mut self) -> io::Result<()> {
        self.flush().await?;
        let mut attempts = 0;
        loop {
            match self.file.sync_data().await {
                Ok(()) => {
                    self.recovering(false);
                    return Ok(());
                }
                Err(error) => self.retry(error, &mut attempts).await?,
            }
        }
    }

    async fn retry(&self, error: io::Error, attempts: &mut u32) -> io::Result<()> {
        if matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData | io::ErrorKind::Unsupported
        ) {
            return Err(error);
        }
        self.recovering(true);
        if *attempts == 0 {
            tracing::warn!("Session file I/O interrupted; retrying the unconfirmed bytes");
        }
        let delay = Duration::from_millis((250u64 << (*attempts).min(5)).min(5000));
        *attempts = attempts.saturating_add(1);
        tokio::time::sleep(delay).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test(start_paused = true)]
    async fn partial_writes_retry_at_the_same_offset_and_header_rewrites_preserve_the_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session");
        let mut writer = ResilientFile::new(File::create(&path).await.unwrap());
        let states = Arc::new(Mutex::new(Vec::new()));
        let observed = states.clone();
        writer.observe(Arc::new(move |active| {
            observed.lock().unwrap().push(active)
        }));
        writer.write_all(b"header-original audio").await.unwrap();
        writer.partial_failures = 2;
        writer.flush().await.unwrap();
        writer.seek(io::SeekFrom::Start(0)).await.unwrap();
        writer.write_all(b"HEADER").await.unwrap();
        writer.partial_failures = 1;
        writer.flush().await.unwrap();
        writer.seek(io::SeekFrom::Start(21)).await.unwrap();
        writer.write_all(b" tail").await.unwrap();
        writer.sync_data().await.unwrap();
        assert_eq!(
            tokio::fs::read(path).await.unwrap(),
            b"HEADER-original audio tail"
        );
        assert!(states.lock().unwrap().iter().any(|state| *state));
        assert_eq!(states.lock().unwrap().last(), Some(&false));
    }
}
