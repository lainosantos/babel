use super::*;

#[tokio::test]
async fn session_cache_uses_unique_system_temporary_directories_and_cleans_up() {
    let first = SessionRetention::create("system-temp-test").await.unwrap();
    let second = SessionRetention::create("system-temp-test").await.unwrap();
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    for store in [&first, &second] {
        let directory = store.directory.path();
        assert_eq!(
            directory.parent().unwrap().canonicalize().unwrap(),
            temporary
        );
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(".babel-retention-")
        );
        let id = store.store(b"source", b"original input").await.unwrap();
        assert_eq!(&*store.load(id).await.unwrap().bytes, b"original input");
    }
    let first_path = first.directory.path().to_owned();
    let second_path = second.directory.path().to_owned();
    assert_ne!(first_path, second_path);
    drop(first);
    assert!(!first_path.exists());
    assert!(second_path.exists());
    drop(second);
    assert!(!second_path.exists());
}

#[tokio::test]
async fn ciphertext_round_trip_preserves_both_lanes_and_ack_is_explicit() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "test-session")
        .await
        .unwrap();
    let microphone = store
        .store(
            br#"{"lane":"microphone","start":123}"#,
            b"original microphone bytes",
        )
        .await
        .unwrap();
    let speaker = store
        .store(
            br#"{"lane":"speaker","start":456}"#,
            b"original speaker bytes",
        )
        .await
        .unwrap();
    let replay = store.clone();
    drop(store);
    let record = replay.load(microphone).await.unwrap();
    assert_eq!(&*record.metadata, br#"{"lane":"microphone","start":123}"#);
    assert_eq!(&*record.bytes, b"original microphone bytes");
    drop(record);
    // A provider attempt can disappear without acknowledging or evicting PCM.
    assert_eq!(
        &*replay.load(microphone).await.unwrap().bytes,
        b"original microphone bytes"
    );
    replay.ack(microphone).await.unwrap();
    replay.ack(microphone).await.unwrap();
    assert!(replay.load(microphone).await.is_err());
    assert_eq!(
        &*replay.load(speaker).await.unwrap().bytes,
        b"original speaker bytes"
    );
    let directory = replay.directory.path().to_owned();
    drop(replay);
    assert!(!directory.exists());
}

#[tokio::test]
async fn disk_contains_no_plaintext_metadata_or_key_and_nonce_changes() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "privacy-test")
        .await
        .unwrap();
    let metadata = b"private source metadata must be encrypted";
    let plaintext = b"private original input must never become a plaintext temporary file";
    let first = store.store(metadata, plaintext).await.unwrap();
    let second = store.store(metadata, plaintext).await.unwrap();
    let first_bytes = fs::read(store.path(first)).unwrap();
    let second_bytes = fs::read(store.path(second)).unwrap();
    assert_ne!(&first_bytes[40..64], &second_bytes[40..64]);
    assert_ne!(&first_bytes[HEADER_BYTES..], &second_bytes[HEADER_BYTES..]);
    let entries: Vec<_> = fs::read_dir(store.directory.path()).unwrap().collect();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(metadata.len())
                .any(|window| window == metadata)
        );
        assert!(
            !bytes
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
        assert!(!bytes.windows(32).any(|window| window == store.key.as_ref()));
        assert!(bytes.starts_with(MAGIC));
    }
}

#[tokio::test]
async fn tampered_headers_ciphertext_and_cross_record_replacement_are_rejected() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "integrity-test")
        .await
        .unwrap();
    let first = store.store(b"source metadata", &[1; 100]).await.unwrap();
    let second = store.store(b"different metadata", &[2; 100]).await.unwrap();
    let path = store.path(first);
    let original = fs::read(&path).unwrap();
    for offset in [0, 8, 24, 32, 40, HEADER_BYTES, original.len() - 1] {
        let mut changed = original.clone();
        changed[offset] ^= 1;
        fs::write(&path, changed).unwrap();
        assert!(
            store.load(first).await.is_err(),
            "accepted changed byte {offset}"
        );
    }
    fs::copy(store.path(second), &path).unwrap();
    assert!(store.load(first).await.is_err());
    fs::write(&path, original).unwrap();
    assert_eq!(&*store.load(first).await.unwrap().bytes, &[1; 100]);
    let other = SessionRetention::create_in(base.path(), "integrity-test")
        .await
        .unwrap();
    assert!(other.load(first).await.is_err());
    assert!(other.ack(first).await.is_err());
    assert!(store.load(first).await.is_ok());
}

#[tokio::test]
async fn torn_extra_and_oversized_records_never_look_complete() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "torn-test")
        .await
        .unwrap();
    let id = store.store(b"", &[5; 128]).await.unwrap();
    let path = store.path(id);
    let original = fs::read(&path).unwrap();
    for length in [0, HEADER_BYTES - 1, HEADER_BYTES, original.len() - 1] {
        fs::write(&path, &original[..length]).unwrap();
        assert!(
            store.load(id).await.is_err(),
            "accepted a torn record of {length} bytes"
        );
    }
    let mut extra = original.clone();
    extra.push(0);
    fs::write(&path, extra).unwrap();
    assert!(store.load(id).await.is_err());
    let mut invalid_length = original.clone();
    invalid_length[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, invalid_length).unwrap();
    assert!(store.load(id).await.is_err());
    fs::write(&path, original).unwrap();
    let file = File::options().write(true).open(&path).unwrap();
    file.set_len((HEADER_BYTES + MAX_ENCRYPTED_BYTES + 1) as u64)
        .unwrap();
    drop(file);
    assert!(store.load(id).await.is_err());
}

#[tokio::test]
async fn bounds_and_invalid_destinations_fail_without_committing_records() {
    let base = tempfile::tempdir().unwrap();
    assert!(
        SessionRetention::create_in(Path::new("relative"), "valid")
            .await
            .is_err()
    );
    assert!(
        SessionRetention::create_in(base.path(), "../unsafe")
            .await
            .is_err()
    );
    let blocked = base.path().join("file");
    fs::write(&blocked, b"not a directory").unwrap();
    assert!(
        SessionRetention::create_in(&blocked.join("child"), "valid")
            .await
            .is_err()
    );
    let store = SessionRetention::create_in(base.path(), "limits")
        .await
        .unwrap();
    assert!(store.store(&[], &[]).await.is_err());
    assert!(
        store
            .store(&vec![0; MAX_METADATA_BYTES + 1], &[1])
            .await
            .is_err()
    );
    assert!(
        store
            .store(&[], &vec![0; MAX_RECORD_BYTES + 1])
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(store.directory.path()).unwrap().count(), 0);
    let id = store.store(&[], &vec![7; MAX_RECORD_BYTES]).await.unwrap();
    assert_eq!(store.load(id).await.unwrap().bytes.len(), MAX_RECORD_BYTES);
}

#[cfg(unix)]
#[tokio::test]
async fn unix_permissions_are_owner_only_from_creation() {
    use std::os::unix::fs::PermissionsExt;
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "permissions")
        .await
        .unwrap();
    let id = store.store(b"", &[1]).await.unwrap();
    assert_eq!(
        fs::metadata(store.directory.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(store.path(id)).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
