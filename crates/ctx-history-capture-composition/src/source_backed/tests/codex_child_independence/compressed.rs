use std::io::Cursor;

use super::*;

fn compressed_session_path(root: &Path, native_session_id: &str) -> PathBuf {
    root.join(format!("rollout-{native_session_id}.jsonl.zst"))
}

fn session_bytes(native_session_id: &str, marker: &str) -> Vec<u8> {
    jsonl_bytes([
        session_meta(
            native_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
        ),
        message(marker),
    ])
}

fn write_compressed(path: &Path, plaintext: &[u8]) {
    let compressed = zstd::stream::encode_all(Cursor::new(plaintext), 1).unwrap();
    fs::write(path, compressed).unwrap();
}

fn logical_identity(records: &[CoreRecord]) -> Vec<(String, String, String)> {
    records
        .iter()
        .map(|record| {
            (
                record.session_id.to_string(),
                record.event_id.to_string(),
                serde_json::to_string(&record.native_event_id).unwrap(),
            )
        })
        .collect()
}

#[test]
fn exact_compressed_and_raw_rollouts_have_identical_logical_identity() {
    let temp = tempdir().unwrap();
    let raw_root = temp.path().join("raw");
    let compressed_root = temp.path().join("compressed");
    let raw_index = temp.path().join("raw-index");
    let compressed_index = temp.path().join("compressed-index");
    fs::create_dir_all(&raw_root).unwrap();
    fs::create_dir_all(&compressed_root).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000061";
    let marker = "compressedrepresentationidentitymarker";
    let plaintext = session_bytes(native_session_id, marker);
    let raw_path = session_path(&raw_root, native_session_id);
    let compressed_path = compressed_session_path(&compressed_root, native_session_id);
    fs::write(&raw_path, &plaintext).unwrap();
    write_compressed(&compressed_path, &plaintext);

    let mut raw_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut raw_registry, &raw_path);
    let raw_receipt =
        refresh_source_backed_generation(&raw_index, &raw_registry, writer_options()).unwrap();
    assert!(raw_receipt.failed_routes.is_empty());

    let mut compressed_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut compressed_registry, &compressed_path);
    let compressed_receipt =
        refresh_source_backed_generation(&compressed_index, &compressed_registry, writer_options())
            .unwrap();
    assert!(compressed_receipt.failed_routes.is_empty());

    let raw = VerifiedIndex::open(&raw_index).unwrap();
    let compressed = VerifiedIndex::open(&compressed_index).unwrap();
    let raw_records = records_for(&raw, native_session_id);
    let compressed_records = records_for(&compressed, native_session_id);
    assert_eq!(raw_records.len(), 1);
    assert_eq!(compressed_records.len(), 1);
    assert_eq!(
        logical_identity(&raw_records),
        logical_identity(&compressed_records)
    );
    assert!(raw_records[0]
        .source
        .exact_descriptor_eq(&compressed_records[0].source));
    assert_eq!(
        raw.search_event_candidates(marker, 8).unwrap()[0]
            .event
            .event_id,
        compressed.search_event_candidates(marker, 8).unwrap()[0]
            .event
            .event_id
    );
}

#[test]
fn mixed_raw_and_compressed_tree_imports_each_native_session() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let raw_id = "019fb000-0000-7000-8000-000000000062";
    let compressed_id = "019fb000-0000-7000-8000-000000000063";
    fs::write(
        session_path(&sessions, raw_id),
        session_bytes(raw_id, "mixedrawmarker"),
    )
    .unwrap();
    write_compressed(
        &compressed_session_path(&sessions, compressed_id),
        &session_bytes(compressed_id, "mixedcompressedmarker"),
    );

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 2);
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&index, raw_id).len(), 1);
    assert_eq!(records_for(&index, compressed_id).len(), 1);
    assert_eq!(
        index
            .search_event_candidates("mixedcompressedmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn raw_to_compressed_representation_transition_replaces_physical_state_only() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000064";
    let marker = "representationtransitionmarker";
    let plaintext = session_bytes(native_session_id, marker);
    let raw_path = session_path(&sessions, native_session_id);
    let compressed_path = compressed_session_path(&sessions, native_session_id);
    fs::write(&raw_path, &plaintext).unwrap();
    let registry = register_tree(&[&sessions]);

    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = VerifiedIndex::open(&index_root).unwrap();
    let before_records = records_for(&before, native_session_id);
    let before_identity = logical_identity(&before_records);
    drop(before);

    write_compressed(&compressed_path, &plaintext);
    fs::remove_file(&raw_path).unwrap();
    let transitioned =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(transitioned.failed_routes.is_empty());
    assert!(transitioned.logical_source_failures.is_empty());
    assert_eq!(transitioned.sources.len(), 1);

    let after = VerifiedIndex::open(&index_root).unwrap();
    let after_records = records_for(&after, native_session_id);
    assert_eq!(logical_identity(&after_records), before_identity);
    assert_eq!(after_records.len(), 1);
    assert_eq!(after.search_event_candidates(marker, 8).unwrap().len(), 1);
    assert_eq!(
        certificate_for(&after, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&compressed_path).unwrap().len()
    );
}

fn assert_compressed_source_rejected(sessions: &Path, index_root: &Path) {
    let registry = register_tree(&[sessions]);
    match refresh_source_backed_generation(index_root, &registry, writer_options()) {
        Ok(receipt) => {
            assert_eq!(receipt.failed_routes.len(), 1);
            assert!(receipt.sources.is_empty());
        }
        Err(SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }) => {
            assert_eq!(failed_routes.len(), 1);
        }
        Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
            assert_eq!(source.kind, SourceBackedRouteErrorKind::InvalidSource);
        }
        Err(error) => panic!("unexpected compressed-source failure: {error:?}"),
    }
}

#[test]
fn corrupt_and_oversize_compressed_rollouts_fail_the_capture_lifecycle() {
    let temp = tempdir().unwrap();
    let corrupt_sessions = temp.path().join("corrupt-sessions");
    fs::create_dir_all(&corrupt_sessions).unwrap();
    let corrupt_id = "019fb000-0000-7000-8000-000000000065";
    let corrupt_path = compressed_session_path(&corrupt_sessions, corrupt_id);
    let mut corrupt = zstd::stream::encode_all(
        Cursor::new(session_bytes(corrupt_id, "corruptcompressedmarker")),
        1,
    )
    .unwrap();
    corrupt[0] ^= 0xff;
    fs::write(corrupt_path, corrupt).unwrap();
    assert_compressed_source_rejected(&corrupt_sessions, &temp.path().join("corrupt-index"));

    let oversize_sessions = temp.path().join("oversize-sessions");
    fs::create_dir_all(&oversize_sessions).unwrap();
    let oversize_id = "019fb000-0000-7000-8000-000000000066";
    let oversize_path = compressed_session_path(&oversize_sessions, oversize_id);
    let bomb_like_plaintext = vec![b' '; 17 * 1024 * 1024];
    write_compressed(&oversize_path, &bomb_like_plaintext);
    assert_compressed_source_rejected(&oversize_sessions, &temp.path().join("oversize-index"));
}
