use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use ctx_history_core::{CoreRecord, EventOrigin, EventRole, EventType, SessionRelationshipKind};
use rusqlite::Connection;

use super::*;
use crate::record_evidence::RecordDigest;

#[test]
fn source_backed_zed_two_threads_project_distinct_sessions_with_complete_core() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = temp.path().join("source");
    fs::create_dir(&source_root).unwrap();
    let database = source_root.join("threads.db");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history/zed/v1/threads.db"),
        &database,
    )
    .unwrap();

    let (scan, records) = project_database(&database, &temp.path().join("data-root"));
    assert_eq!(scan.counters.sessions_retained, 2);
    assert_eq!(scan.counters.retained_events, 5);
    assert_eq!(records.len(), 5);

    let source = zed_source_key().unwrap();
    let sessions = records
        .iter()
        .map(|record| {
            (
                record.provider_session_id.clone().unwrap(),
                record.session_id,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions["zed-root"],
        zed_session_identity(&source, "zed-root").unwrap()
    );
    assert_eq!(
        sessions["zed-child"],
        zed_session_identity(&source, "zed-child").unwrap()
    );
    assert_eq!(
        sessions["zed-root"].to_string(),
        "9297e773-a7a9-8d7b-bb47-fd24429fa1fc"
    );
    assert_eq!(
        sessions["zed-child"].to_string(),
        "c0b6d44d-f2ec-8655-8b9c-1dbf4df37d9f"
    );
    assert_ne!(sessions["zed-root"], sessions["zed-child"]);

    let child_records = records
        .iter()
        .filter(|record| record.provider_session_id.as_deref() == Some("zed-child"))
        .collect::<Vec<_>>();
    assert!(!child_records.is_empty());
    assert!(child_records.iter().all(|record| {
        record.session_relationship == SessionRelationshipKind::Delegated
            && record.event_origin == EventOrigin::UniqueToSession
            && !record.is_primary
            && record.parent_session_id == Some(sessions["zed-root"])
            && record.root_session_id == sessions["zed-root"]
    }));
    let root_records = records
        .iter()
        .filter(|record| record.provider_session_id.as_deref() == Some("zed-root"))
        .collect::<Vec<_>>();
    assert!(!root_records.is_empty());
    assert!(root_records.iter().all(|record| {
        record.session_relationship == SessionRelationshipKind::Root
            && record.event_origin == EventOrigin::Unknown
            && record.is_primary
            && record.parent_session_id.is_none()
            && record.root_session_id == sessions["zed-root"]
    }));

    let event_ids = records
        .iter()
        .map(|record| {
            (
                (
                    record.provider_session_id.clone().unwrap(),
                    record.event_sequence,
                ),
                record.event_id.to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        event_ids[&("zed-child".to_owned(), 0)],
        "ff762302-0f1a-8f62-9444-7e77fd867833"
    );
    assert_eq!(
        event_ids[&("zed-child".to_owned(), 2)],
        "10589418-38f7-88b3-8245-39c5814021d8"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 0)],
        "79a8c6e8-2811-88c8-9698-46e38553ed4d"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 2)],
        "1ad66a77-b057-8ae9-b94b-00d525101137"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 4)],
        "bea728bb-1983-8ac5-9e04-e75259b71e33"
    );

    let bodies = records
        .iter()
        .map(|record| record.content.meaningful_text().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        bodies,
        BTreeSet::from([
            "zed child oracle answer".to_owned(),
            "zed child oracle prompt".to_owned(),
            "zed compacted summary oracle".to_owned(),
            "zed sqlite oracle answer\ntool call: write_file\ntool input: present".to_owned(),
            "zed sqlite oracle prompt".to_owned(),
        ])
    );
    let tool_call = records
        .iter()
        .find(|record| record.event_type == EventType::ToolCall.as_str())
        .expect("fixture contains one retained Zed tool call");
    assert_eq!(
        tool_call.role.as_deref(),
        Some(EventRole::Assistant.as_str())
    );
    assert!(tool_call.native_event_id.is_some());
    let native_parts = tool_call
        .content
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/native_message/content/content"))
        .and_then(serde_json::Value::as_array)
        .expect("tool call retains its decoded native content");
    let native_tool = native_parts
        .iter()
        .find(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
        .expect("tool call retains structured tool input");
    assert_eq!(
        native_tool.get("name").and_then(serde_json::Value::as_str),
        Some("write_file")
    );
    assert!(native_tool
        .get("input")
        .is_some_and(|value| !value.is_null()));
}

#[test]
fn zed_core_record_retains_full_tail_beyond_sixteen_kibibytes() {
    const TAIL: &str = "zedpostsixteenkilobytesentinel";

    let source = zed_source_key().unwrap();
    let context = root_context(&source, "thread-full-body");
    let full_body = format!(
        r#"{{"arguments":{{"padding":"{}","tail":"{TAIL}"}},"tool":"write_file"}}"#,
        "x".repeat(17_000)
    );
    assert!(full_body.find(TAIL).unwrap() > 16 * 1_024);
    let event = ZedNativeEvent::from_draft(
        1,
        "thread-full-body",
        super::super::model::ZedDecodedCoreEvent {
            provider_message_id: Some("message-full-body".to_owned()),
            thread_ordinal: 0,
            message_ordinal: 0,
            event_type: EventType::Message,
            role: EventRole::User,
            occurred_at: "2026-07-28T12:00:01Z".parse().unwrap(),
            kind: "user",
            call_ids: Vec::new(),
            native_content: serde_json::json!({
                "kind": "user",
                "content": [{"type": "text"}],
            }),
            body: full_body.clone(),
            safe_file_touches: Vec::new(),
        },
        RecordDigest::from_text(&full_body),
    )
    .unwrap();
    let record = zed_core_record(&source, &context, event).unwrap();
    assert_eq!(record.content.meaningful_text(), full_body);
    assert_eq!(
        record
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/native_message/content/content/0/type"))
            .and_then(serde_json::Value::as_str),
        Some("text")
    );
    let structured: serde_json::Value =
        serde_json::from_str(record.content.meaningful_text()).unwrap();
    assert_eq!(
        structured
            .pointer("/arguments/tail")
            .and_then(serde_json::Value::as_str),
        Some(TAIL)
    );
    let encoded = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains("\"locator\""));
    assert!(!encoded.contains("\"source_path\""));
}

#[test]
fn pinned_zed_core_survives_source_movement_and_change() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("threads.db");
    let moved = temp.path().join("threads-moved.db");
    let full_body = format!("zed-head-{}-zed-tail", "z".repeat(20_000));
    super::tests::create_database(&database, &full_body);

    let (_, records) = project_database(&database, &temp.path().join("data-root"));
    let [record] = records.as_slice() else {
        panic!("expected one pinned Zed record");
    };
    let event_id = record.event_id;
    let session_id = record.session_id;
    let pinned = record.encode_stored().unwrap();
    assert_eq!(record.content.meaningful_text(), full_body);

    fs::rename(&database, &moved).unwrap();
    assert_pinned_core(&pinned, event_id, session_id, &full_body);
    fs::rename(&moved, &database).unwrap();
    super::tests::replace_thread(&database, "zed changed source body");
    assert_pinned_core(&pinned, event_id, session_id, &full_body);

    let (_, rewritten) = project_database(&database, &temp.path().join("next-data-root"));
    let [rewritten] = rewritten.as_slice() else {
        panic!("expected one rewritten Zed record");
    };
    assert_eq!(rewritten.event_id, event_id);
    assert_eq!(rewritten.session_id, session_id);
    assert_eq!(
        rewritten.content.meaningful_text(),
        "zed changed source body"
    );
}

#[test]
fn checkpoint_vacuum_and_shm_churn_do_not_change_logical_revision_or_projection() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("threads.db");
    let data_root = temp.path().join("data-root");
    super::tests::create_database(&database, "logical no-op sentinel");
    let writer = Connection::open(&database).unwrap();
    writer
        .execute("update threads set rowid = 42 where id = 'thread-1'", [])
        .unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "begin immediate;
             update threads set rowid = 43 where id = 'thread-1';
             update threads set rowid = 42 where id = 'thread-1';
             commit;",
        )
        .unwrap();

    let (cold_revision, cold_scan, cold_records) =
        project_database_with_revision(&database, &data_root);
    assert_eq!(cold_scan.counters.sessions_retained, 1);
    assert_eq!(cold_scan.counters.retained_events, 1);
    let [cold] = cold_records.as_slice() else {
        panic!("expected one Zed Core record");
    };

    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    let (checkpoint_revision, checkpoint_scan, checkpoint_records) =
        project_database_with_revision(&database, &data_root);
    assert_eq!(checkpoint_revision, cold_revision);
    assert_eq!(
        checkpoint_scan.source_integrity_digest,
        cold_scan.source_integrity_digest
    );
    let [checkpoint] = checkpoint_records.as_slice() else {
        panic!("expected one Zed checkpoint record");
    };
    assert_same_semantic_projection(cold, checkpoint);

    writer
        .execute("update threads set rowid = 84 where id = 'thread-1'", [])
        .unwrap();
    writer.execute_batch("vacuum").unwrap();
    let vacuumed_rowid = writer
        .query_row(
            "select rowid from threads where id = 'thread-1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_ne!(vacuumed_rowid, 42);
    let (vacuum_revision, vacuum_scan, vacuum_records) =
        project_database_with_revision(&database, &data_root);
    assert_eq!(vacuum_revision, cold_revision);
    assert_eq!(
        vacuum_scan.source_integrity_digest,
        cold_scan.source_integrity_digest
    );
    let [vacuum] = vacuum_records.as_slice() else {
        panic!("expected one Zed vacuum record");
    };
    assert_same_semantic_projection(cold, vacuum);

    let shm = sqlite_component_path(&database, "-shm");
    let before_shm = fs::read(&shm).unwrap();
    rewrite_same_shm_bytes(&shm);
    assert_eq!(fs::read(&shm).unwrap(), before_shm);
    let (shm_revision, shm_scan, shm_records) =
        project_database_with_revision(&database, &data_root);
    assert_eq!(shm_revision, cold_revision);
    assert_eq!(
        shm_scan.source_integrity_digest,
        cold_scan.source_integrity_digest
    );
    let [shm] = shm_records.as_slice() else {
        panic!("expected one Zed shm-churn record");
    };
    assert_same_semantic_projection(cold, shm);
}

fn project_database(
    database: &Path,
    data_root: &Path,
) -> (
    super::super::query::ZedNativeQueryResult,
    Vec<ctx_history_core::CoreRecord>,
) {
    let mut snapshot = acquire_snapshot(data_root, database).unwrap();
    let revision = snapshot.snapshot_revision.clone();
    let source = zed_source_key().unwrap();
    let mut records = Vec::new();
    let scan;
    {
        let mut sink =
            ZedSourceBackedSinkV0::with_emitter(snapshot.connection().unwrap(), source, |record| {
                records.push(record);
                Ok(())
            })
            .unwrap();
        scan =
            scan_zed_native_snapshot(snapshot.connection().unwrap(), &revision, &mut sink).unwrap();
        assert!(sink.take_failure().is_none());
        assert_eq!(sink.staged_core_records(), scan.counters.retained_events);
    }
    snapshot.finish().unwrap();
    (scan, records)
}

fn project_database_with_revision(
    database: &Path,
    data_root: &Path,
) -> (
    String,
    super::super::query::ZedNativeQueryResult,
    Vec<CoreRecord>,
) {
    let mut snapshot = acquire_snapshot(data_root, database).unwrap();
    let revision = snapshot.snapshot_revision.clone();
    let source = zed_source_key().unwrap();
    let mut records = Vec::new();
    let scan;
    {
        let mut sink =
            ZedSourceBackedSinkV0::with_emitter(snapshot.connection().unwrap(), source, |record| {
                records.push(record);
                Ok(())
            })
            .unwrap();
        scan =
            scan_zed_native_snapshot(snapshot.connection().unwrap(), &revision, &mut sink).unwrap();
        assert!(sink.take_failure().is_none());
        assert_eq!(sink.staged_core_records(), scan.counters.retained_events);
    }
    snapshot.finish().unwrap();
    (revision, scan, records)
}

fn assert_same_semantic_projection(expected: &CoreRecord, actual: &CoreRecord) {
    assert_eq!(actual.event_id, expected.event_id);
    assert_eq!(actual.session_id, expected.session_id);
    assert_eq!(actual.parent_session_id, expected.parent_session_id);
    assert_eq!(actual.root_session_id, expected.root_session_id);
    assert_eq!(actual.provider_session_id, expected.provider_session_id);
    assert_eq!(actual.native_event_id, expected.native_event_id);
    assert_eq!(actual.event_sequence, expected.event_sequence);
    assert_eq!(actual.event_type, expected.event_type);
    assert_eq!(actual.role, expected.role);
    assert_eq!(actual.session_relationship, expected.session_relationship);
    assert_eq!(actual.event_origin, expected.event_origin);
    assert_eq!(
        actual.content.meaningful_text(),
        expected.content.meaningful_text()
    );
}

fn root_context(
    source: &ctx_history_core::SourceKey,
    thread_id: &str,
) -> ZedSessionProjectionContextV0 {
    let session_id = zed_session_identity(source, thread_id).unwrap();
    ZedSessionProjectionContextV0 {
        session: super::super::dto::ZedNativeSession {
            sqlite_rowid: 1,
            thread_id: thread_id.to_owned(),
            parent_thread_id: None,
            title: "Full body".to_owned(),
            payload_title: Some("Full body".to_owned()),
            summary: String::new(),
            created_at: "2026-07-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-07-28T12:00:01Z".parse().unwrap(),
            native_created_at: Some("2026-07-28T12:00:00Z".to_owned()),
            native_updated_at: "2026-07-28T12:00:01Z".to_owned(),
            cwd: Some("/workspace/zed".to_owned()),
            folder_paths: vec!["/workspace/zed".to_owned()],
            native_folder_paths: Some("/workspace/zed".to_owned()),
            native_folder_paths_order: Some("0".to_owned()),
            native_data_type: "json".to_owned(),
            encoding: super::super::dto::ZedNativeEncoding::Json,
        },
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        root_thread_id: thread_id.to_owned(),
    }
}

fn assert_pinned_core(
    encoded: &[u8],
    event_id: ctx_history_core::StableEntityId,
    session_id: ctx_history_core::StableEntityId,
    expected: &str,
) {
    let record = ctx_history_core::CoreRecord::decode_stored(encoded).unwrap();
    assert_eq!(record.event_id, event_id);
    assert_eq!(record.session_id, session_id);
    assert_eq!(record.content.meaningful_text(), expected);
}

fn sqlite_component_path(database: &Path, suffix: &str) -> PathBuf {
    let mut component = database.as_os_str().to_os_string();
    component.push(suffix);
    PathBuf::from(component)
}

fn rewrite_same_shm_bytes(path: &Path) {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}
