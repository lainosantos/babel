//! Encrypted, ephemeral retention for original processing input.
//!
//! Callers batch frames and await storage before submitting them to inference.
//! These APIs must never run on an audio callback or the original routing
//! executor. Only ciphertext reaches disk; the per-store key lives in RAM and
//! is erased when the last owner disappears. This deliberately cannot recover
//! after process exit. OS swap and crash dumps are outside this API's control.
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, ensure};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{AeadInPlace, OsRng, rand_core::RngCore},
};
use tempfile::TempDir;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

pub const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 4096;
const MAGIC: &[u8; 8] = b"BABELR01";
const HEADER_BYTES: usize = 64;
const TAG_BYTES: usize = 16;
const MAX_ENCRYPTED_BYTES: usize = MAX_RECORD_BYTES + MAX_METADATA_BYTES + 4 + TAG_BYTES;

/// An opaque ID is meaningful only for the store that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordId {
    store: [u8; 16],
    sequence: u64,
}

/// Decrypted buffers are cleared when dropped and intentionally have no Debug.
pub struct RetainedRecord {
    pub metadata: Zeroizing<Vec<u8>>,
    pub bytes: Zeroizing<Vec<u8>>,
}

/// Keep an Arc in session/archive ownership until processing and text storage
/// have committed. A provider task must not be the only owner of this object.
/// No automatic age, size, or acknowledgment eviction occurs.
pub struct SessionRetention {
    key: Zeroizing<[u8; 32]>,
    directory: TempDir,
    identity: [u8; 16],
    session: Box<str>,
    next: AtomicU64,
    operations: Arc<Semaphore>,
}

impl SessionRetention {
    /// Temporary originals are independent of the configured final output path.
    pub async fn create(session: &str) -> Result<Arc<Self>> {
        Self::create_in(&std::env::temp_dir(), session).await
    }

    pub(crate) async fn create_in(base: &Path, session: &str) -> Result<Arc<Self>> {
        ensure!(
            base.is_absolute(),
            "Retention temporary path must be absolute"
        );
        ensure!(
            !session.is_empty()
                && session.len() <= 128
                && session
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
            "Invalid retention session identifier"
        );
        let base = base.to_owned();
        let session: Box<str> = session.into();
        tokio::task::spawn_blocking(move || {
            let mut builder = tempfile::Builder::new();
            builder.prefix(".babel-retention-");
            // Windows inherits the system temporary directory's ACL. The
            // encrypted payload is protected independently of filesystem ACLs.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                builder.permissions(fs::Permissions::from_mode(0o700));
            }
            let directory = builder
                .tempdir_in(base)
                .context("Could not create temporary retention folder")?;
            let mut key = Zeroizing::new([0u8; 32]);
            OsRng
                .try_fill_bytes(key.as_mut())
                .map_err(|_| anyhow!("Retention key generation failed"))?;
            let mut identity = [0u8; 16];
            OsRng
                .try_fill_bytes(&mut identity)
                .map_err(|_| anyhow!("Retention identity generation failed"))?;
            Ok(Arc::new(Self {
                key,
                directory,
                identity,
                session,
                next: AtomicU64::new(0),
                operations: Arc::new(Semaphore::new(2)),
            }))
        })
        .await
        .context("Retention initialization worker failed")?
    }

    /// Store a caller-batched original, including opaque metadata, atomically.
    /// Success means encrypted bytes were written and synced. If the awaiting
    /// task is cancelled, its blocking operation may finish; the store still
    /// owns the encrypted file and removes it when its final owner is dropped.
    pub async fn store(self: &Arc<Self>, metadata: &[u8], bytes: &[u8]) -> Result<RecordId> {
        ensure!(
            metadata.len() <= MAX_METADATA_BYTES,
            "Retention metadata exceeds the limit"
        );
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_RECORD_BYTES,
            "Retention payload size is invalid"
        );
        // Bound concurrent copies/encryption without retaining plaintext in a
        // queued blocking job for every incoming frame.
        let permit = self
            .operations
            .clone()
            .acquire_owned()
            .await
            .context("Retention worker unavailable")?;
        let mut plaintext = Zeroizing::new(Vec::with_capacity(
            4 + metadata.len() + bytes.len() + TAG_BYTES,
        ));
        plaintext.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        plaintext.extend_from_slice(metadata);
        plaintext.extend_from_slice(bytes);
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.store_blocking(plaintext)
        })
        .await
        .context("Retention storage worker failed")?
    }

    pub async fn load(self: &Arc<Self>, id: RecordId) -> Result<RetainedRecord> {
        self.validate_id(id)?;
        let permit = self
            .operations
            .clone()
            .acquire_owned()
            .await
            .context("Retention worker unavailable")?;
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.load_blocking(id)
        })
        .await
        .context("Retention read worker failed")?
    }

    /// Delete only after the caller has committed its corresponding result.
    /// This is explicit and idempotent; failed processing must never call ack.
    pub async fn ack(self: &Arc<Self>, id: RecordId) -> Result<()> {
        self.validate_id(id)?;
        let store = self.clone();
        tokio::task::spawn_blocking(move || match fs::remove_file(store.path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Could not remove acknowledged retention record"),
        })
        .await
        .context("Retention cleanup worker failed")?
    }

    fn validate_id(&self, id: RecordId) -> Result<()> {
        ensure!(
            id.store == self.identity,
            "Retention record belongs to another session"
        );
        Ok(())
    }

    fn path(&self, id: RecordId) -> PathBuf {
        self.directory
            .path()
            .join(format!("{:016x}.brc", id.sequence))
    }

    fn aad(&self, header: &[u8]) -> Vec<u8> {
        let mut aad = Vec::with_capacity(header.len() + self.session.len());
        aad.extend_from_slice(header);
        aad.extend_from_slice(self.session.as_bytes());
        aad
    }

    fn store_blocking(&self, mut plaintext: Zeroizing<Vec<u8>>) -> Result<RecordId> {
        let sequence = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| anyhow!("Retention record identifier exhausted"))?;
        let id = RecordId {
            store: self.identity,
            sequence,
        };
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(MAGIC);
        header[8..24].copy_from_slice(&self.identity);
        header[24..32].copy_from_slice(&sequence.to_le_bytes());
        header[32..40].copy_from_slice(&(plaintext.len() as u64).to_le_bytes());
        OsRng
            .try_fill_bytes(&mut header[40..64])
            .map_err(|_| anyhow!("Retention nonce generation failed"))?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| anyhow!("Retention cipher initialization failed"))?;
        cipher
            .encrypt_in_place(
                XNonce::from_slice(&header[40..64]),
                &self.aad(&header),
                &mut *plaintext,
            )
            .map_err(|_| anyhow!("Retention encryption failed"))?;
        // The temporary file contains ciphertext from its very first write.
        let mut file = tempfile::Builder::new()
            .prefix(".writing-")
            .tempfile_in(self.directory.path())
            .context("Could not create encrypted retention record")?;
        file.write_all(&header)
            .context("Could not write retention header")?;
        file.write_all(&plaintext)
            .context("Could not write encrypted retention payload")?;
        file.as_file()
            .sync_all()
            .context("Could not sync encrypted retention record")?;
        file.persist_noclobber(self.path(id))
            .map_err(|_| anyhow!("Could not commit encrypted retention record"))?;
        Ok(id)
    }

    fn load_blocking(&self, id: RecordId) -> Result<RetainedRecord> {
        let mut file =
            File::open(self.path(id)).context("Could not open encrypted retention record")?;
        let size = file
            .metadata()
            .context("Could not inspect encrypted retention record")?
            .len();
        ensure!(
            (HEADER_BYTES + TAG_BYTES + 5) as u64 <= size
                && size <= (HEADER_BYTES + MAX_ENCRYPTED_BYTES) as u64,
            "Encrypted retention record size is invalid"
        );
        let mut header = [0u8; HEADER_BYTES];
        file.read_exact(&mut header)
            .context("Encrypted retention header is truncated")?;
        ensure!(
            &header[..8] == MAGIC
                && header[8..24] == self.identity
                && header[24..32] == id.sequence.to_le_bytes(),
            "Encrypted retention header is invalid"
        );
        let payload_len = u64::from_le_bytes(header[32..40].try_into().unwrap());
        ensure!(
            payload_len >= 5
                && payload_len <= (MAX_ENCRYPTED_BYTES - TAG_BYTES) as u64
                && size == HEADER_BYTES as u64 + payload_len + TAG_BYTES as u64,
            "Encrypted retention payload length is invalid"
        );
        let mut payload = Zeroizing::new(vec![0; payload_len as usize + TAG_BYTES]);
        file.read_exact(&mut payload)
            .context("Encrypted retention payload is truncated")?;
        let mut trailing = [0u8; 1];
        ensure!(
            file.read(&mut trailing)
                .context("Could not finish reading encrypted retention record")?
                == 0,
            "Encrypted retention record has trailing data"
        );
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| anyhow!("Retention cipher initialization failed"))?;
        cipher
            .decrypt_in_place(
                XNonce::from_slice(&header[40..64]),
                &self.aad(&header),
                &mut *payload,
            )
            .map_err(|_| anyhow!("Encrypted retention record authentication failed"))?;
        let metadata_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        ensure!(
            metadata_len <= MAX_METADATA_BYTES
                && metadata_len + 4 < payload.len()
                && payload.len() - metadata_len - 4 <= MAX_RECORD_BYTES,
            "Decrypted retention record lengths are invalid"
        );
        Ok(RetainedRecord {
            metadata: Zeroizing::new(payload[4..4 + metadata_len].to_vec()),
            bytes: Zeroizing::new(payload[4 + metadata_len..].to_vec()),
        })
    }
}

#[cfg(test)]
mod tests;
