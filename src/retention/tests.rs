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

#[tokio::test]
async fn journal_many_records_share_one_file_and_coexist_with_standalone_records() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-many")
        .await
        .unwrap();
    let mut records = Vec::new();
    for index in 0..64u8 {
        let metadata = format!("source-frame-{index}").into_bytes();
        let bytes = vec![index; 37];
        let id = store.append(&metadata, &bytes).await.unwrap();
        records.push((id, metadata, bytes));
    }
    let files: Vec<_> = fs::read_dir(store.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files, vec![store.journal_path()]);
    assert_eq!(store.journal_path().file_name().unwrap(), "session.brj");

    let standalone = store
        .store(b"standalone metadata", b"standalone original")
        .await
        .unwrap();
    assert_eq!(fs::read_dir(store.directory.path()).unwrap().count(), 2);
    assert!(records.iter().all(|(id, _, _)| *id != standalone));
    for (id, metadata, bytes) in &records {
        let loaded = store.load(*id).await.unwrap();
        assert_eq!(&*loaded.metadata, metadata);
        assert_eq!(&*loaded.bytes, bytes);
    }
    assert_eq!(
        &*store.load(standalone).await.unwrap().bytes,
        b"standalone original"
    );
    store.ack(standalone).await.unwrap();
    assert!(!store.path(standalone).exists());
    assert!(store.journal_path().exists());
    assert_eq!(
        &*store.load(records[0].0).await.unwrap().bytes,
        &records[0].2
    );
    let directory = store.directory.path().to_owned();
    drop(store);
    assert!(!directory.exists());
}

#[tokio::test]
async fn journal_encrypts_metadata_and_plaintext_with_unique_record_nonces() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-privacy")
        .await
        .unwrap();
    let metadata = b"private journal metadata must remain encrypted";
    let plaintext = b"private journal source audio must never be written as plaintext";
    for _ in 0..32 {
        store.append(metadata, plaintext).await.unwrap();
    }
    let journal = fs::read(store.journal_path()).unwrap();
    for secret in [
        metadata.as_slice(),
        plaintext.as_slice(),
        store.key.as_ref(),
    ] {
        assert!(!journal.windows(secret.len()).any(|window| window == secret));
    }
    let record_length = HEADER_BYTES + 4 + metadata.len() + plaintext.len() + TAG_BYTES;
    assert_eq!(journal.len(), record_length * 32);
    let mut nonces = std::collections::HashSet::new();
    let mut ciphertexts = std::collections::HashSet::new();
    for record in journal.chunks_exact(record_length) {
        assert!(record.starts_with(MAGIC));
        assert!(nonces.insert(record[40..64].to_vec()));
        assert!(ciphertexts.insert(record[HEADER_BYTES..].to_vec()));
    }
    assert_eq!(nonces.len(), 32);
}

#[tokio::test]
async fn journal_authenticates_headers_metadata_session_and_record_identity() {
    let base = tempfile::tempdir().unwrap();
    let mut store = SessionRetention::create_in(base.path(), "journal-integrity")
        .await
        .unwrap();
    let first = store.append(b"source metadata", &[1; 100]).await.unwrap();
    let second = store.append(b"source metadata", &[2; 100]).await.unwrap();
    let path = store.journal_path();
    let original = fs::read(&path).unwrap();
    let record_length = original.len() / 2;
    for relative in [
        0,
        8,
        24,
        32,
        40,
        HEADER_BYTES,
        HEADER_BYTES + 4,
        HEADER_BYTES + 4 + b"source metadata".len(),
        record_length - 1,
    ] {
        let mut changed = original.clone();
        changed[record_length + relative] ^= 1;
        fs::write(&path, changed).unwrap();
        assert!(
            store.load(second).await.is_err(),
            "accepted changed journal byte {relative}"
        );
        assert_eq!(&*store.load(first).await.unwrap().bytes, &[1; 100]);
    }
    let mut replaced = original.clone();
    replaced.copy_within(..record_length, record_length);
    fs::write(&path, replaced).unwrap();
    assert!(store.load(second).await.is_err());
    assert_eq!(&*store.load(first).await.unwrap().bytes, &[1; 100]);
    fs::write(&path, &original).unwrap();

    // The session is authenticated even when key, store ID, header and payload
    // are unchanged, so a journal cannot be reassigned to another session.
    let session = store.session.clone();
    Arc::get_mut(&mut store).unwrap().session = "different-session".into();
    assert!(store.load(first).await.is_err());
    assert!(store.load(second).await.is_err());
    Arc::get_mut(&mut store).unwrap().session = session;
    assert_eq!(&*store.load(second).await.unwrap().bytes, &[2; 100]);

    let other = SessionRetention::create_in(base.path(), "journal-integrity")
        .await
        .unwrap();
    assert!(other.load(first).await.is_err());
    assert!(other.ack(first).await.is_err());
    assert!(store.load(first).await.is_ok());
}

#[tokio::test]
async fn journal_concurrent_appends_and_loads_preserve_every_record() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-concurrent")
        .await
        .unwrap();
    let mut workers = tokio::task::JoinSet::new();
    for index in 0..32u64 {
        let store = store.clone();
        workers.spawn(async move {
            let metadata = format!("concurrent-source-{index}").into_bytes();
            let bytes = index.to_le_bytes().repeat(64);
            let id = store.append(&metadata, &bytes).await.unwrap();
            let loaded = store.load(id).await.unwrap();
            assert_eq!(&*loaded.metadata, &metadata);
            assert_eq!(&*loaded.bytes, &bytes);
            (id, metadata, bytes)
        });
    }
    let mut sequences = std::collections::HashSet::new();
    while let Some(result) = workers.join_next().await {
        let (id, metadata, bytes) = result.unwrap();
        assert!(sequences.insert(id.sequence));
        let loaded = store.load(id).await.unwrap();
        assert_eq!(&*loaded.metadata, &metadata);
        assert_eq!(&*loaded.bytes, &bytes);
    }
    assert_eq!(sequences.len(), 32);
    assert_eq!(fs::read_dir(store.directory.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn journal_ack_is_logical_idempotent_and_preserves_other_records() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-ack")
        .await
        .unwrap();
    let first = store.append(b"first", &[1; 100]).await.unwrap();
    let second = store.append(b"second", &[2; 100]).await.unwrap();
    let path = store.journal_path();
    let committed = fs::read(&path).unwrap();
    store.ack(first).await.unwrap();
    store.ack(first).await.unwrap();
    assert!(store.load(first).await.is_err());
    assert_eq!(fs::read(&path).unwrap(), committed);
    assert_eq!(&*store.load(second).await.unwrap().bytes, &[2; 100]);

    let third = store.append(b"third", &[3; 100]).await.unwrap();
    assert!(store.load(first).await.is_err());
    assert_eq!(&*store.load(second).await.unwrap().bytes, &[2; 100]);
    assert_eq!(&*store.load(third).await.unwrap().bytes, &[3; 100]);
    store.ack(second).await.unwrap();
    store.ack(third).await.unwrap();
    assert!(store.load(second).await.is_err());
    assert!(store.load(third).await.is_err());
    assert!(!path.exists());
    store.ack(third).await.unwrap();
    let fourth = store.append(b"fourth", &[4; 100]).await.unwrap();
    assert!(fourth.sequence > third.sequence);
    assert_eq!(&*store.load(fourth).await.unwrap().bytes, &[4; 100]);
    assert!(store.load(first).await.is_err());
    assert!(store.load(second).await.is_err());
    assert!(store.load(third).await.is_err());
}

#[tokio::test]
async fn journal_truncated_later_record_does_not_hide_earlier_committed_record() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-truncation")
        .await
        .unwrap();
    let first = store.append(b"first", &[1; 100]).await.unwrap();
    let first_end = fs::metadata(store.journal_path()).unwrap().len() as usize;
    let second = store.append(b"second", &[2; 100]).await.unwrap();
    let path = store.journal_path();
    let original = fs::read(&path).unwrap();
    for end in [
        first_end,
        first_end + HEADER_BYTES - 1,
        first_end + HEADER_BYTES,
        original.len() - 1,
    ] {
        fs::write(&path, &original[..end]).unwrap();
        assert!(
            store.load(second).await.is_err(),
            "accepted journal truncated at {end}"
        );
        assert_eq!(&*store.load(first).await.unwrap().bytes, &[1; 100]);
    }
    fs::write(&path, original).unwrap();
    assert_eq!(&*store.load(second).await.unwrap().bytes, &[2; 100]);
}

#[tokio::test]
async fn journal_next_append_removes_uncommitted_tail_without_losing_prior_records() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-aborted-tail")
        .await
        .unwrap();
    let first = store.append(b"first", &[1; 100]).await.unwrap();
    let second = store.append(b"second", &[2; 100]).await.unwrap();
    let path = store.journal_path();
    let committed = fs::read(&path).unwrap();
    // Simulate a write interrupted after ciphertext reached disk, before its
    // offset was committed to the in-memory journal index.
    let mut file = File::options().append(true).open(&path).unwrap();
    file.write_all(&[0xa5; 777]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(&*store.load(first).await.unwrap().bytes, &[1; 100]);
    assert_eq!(&*store.load(second).await.unwrap().bytes, &[2; 100]);

    let third = store.append(b"third", &[3; 100]).await.unwrap();
    let journal = fs::read(&path).unwrap();
    let expected_new_length = HEADER_BYTES + 4 + b"third".len() + 100 + TAG_BYTES;
    assert_eq!(journal.len(), committed.len() + expected_new_length);
    assert_eq!(&journal[..committed.len()], &committed);
    for (id, byte) in [(first, 1), (second, 2), (third, 3)] {
        assert_eq!(&*store.load(id).await.unwrap().bytes, &[byte; 100]);
    }
}

#[tokio::test]
async fn journal_rejects_invalid_record_sizes_before_committing() {
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-limits")
        .await
        .unwrap();
    assert!(store.append(&[], &[]).await.is_err());
    assert!(
        store
            .append(&vec![0; MAX_METADATA_BYTES + 1], &[1])
            .await
            .is_err()
    );
    assert!(
        store
            .append(&[], &vec![0; MAX_RECORD_BYTES + 1])
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(store.directory.path()).unwrap().count(), 0);
    let id = store.append(b"valid", &[7; 37]).await.unwrap();
    assert_eq!(&*store.load(id).await.unwrap().bytes, &[7; 37]);
}

#[cfg(unix)]
#[tokio::test]
async fn journal_file_permissions_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let base = tempfile::tempdir().unwrap();
    let store = SessionRetention::create_in(base.path(), "journal-permissions")
        .await
        .unwrap();
    store.append(b"source", &[1]).await.unwrap();
    assert_eq!(
        fs::metadata(store.journal_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
