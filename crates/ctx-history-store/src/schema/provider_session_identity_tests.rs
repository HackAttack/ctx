use std::fs;

use ctx_history_core::{new_id, CaptureProvider};
use rusqlite::{params, Connection};

use crate::schema::ddl::CREATE_TABLES_SQL;
use crate::schema::fts::FTS_TABLES_SQL;
use crate::schema::indexes::INDEXES_SQL;
use crate::Store;

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-provider-session-identity-")
        .tempdir_in(root)
        .unwrap()
}

#[test]
fn schema_v47_repairs_same_path_provider_sessions_and_preserves_id_aliases() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let old_source_id = new_id();
    let new_source_id = new_id();
    let other_source_id = new_id();
    let old_session_id = new_id();
    let duplicate_session_id = new_id();
    let other_session_id = new_id();
    let old_event_id = new_id();
    let duplicate_event_id = new_id();
    let appended_event_id = new_id();
    let file_touch_id = new_id();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(CREATE_TABLES_SQL).unwrap();
        conn.execute_batch(FTS_TABLES_SQL).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("DROP TABLE event_aliases; DROP TABLE session_aliases;")
            .unwrap();
        for (id, path, source_format, source_identity) in [
            (old_source_id, "/tmp/claude/session.jsonl", None, None),
            (
                new_source_id,
                "/tmp/claude/session.jsonl",
                Some("claude_projects_jsonl_tree"),
                Some("source-identity"),
            ),
            (
                other_source_id,
                "/tmp/claude/copied/session.jsonl",
                Some("claude_projects_jsonl_tree"),
                Some("other-source-identity"),
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format,
                 source_root, source_identity, external_session_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'claude', 'test-machine', ?2, ?3,
                        ?2, ?4, 'shared-provider-id', 0, 'imported')
                "#,
                params![id.to_string(), path, source_format, source_identity],
            )
            .unwrap();
        }
        for (id, source_id, created_at_ms) in [
            (old_session_id, old_source_id, 1),
            (duplicate_session_id, new_source_id, 2),
            (other_session_id, other_source_id, 3),
        ] {
            conn.execute(
                r#"
                INSERT INTO sessions
                (id, capture_source_id, provider, external_session_id, agent_type,
                 is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, 'claude', 'shared-provider-id', 'primary',
                        1, 'imported', 'imported', 0, ?3, ?3)
                "#,
                params![id.to_string(), source_id.to_string(), created_at_ms],
            )
            .unwrap();
        }
        for (id, seq, session_id, source_id, provider_index, provider_hash, dedupe_key) in [
            (
                old_event_id,
                1,
                old_session_id,
                old_source_id,
                0,
                "event-0",
                "provider:claude:shared-provider-id:0:event-0",
            ),
            (
                duplicate_event_id,
                2,
                duplicate_session_id,
                new_source_id,
                0,
                "event-0",
                "provider-source:new-source:0:event-0",
            ),
            (
                appended_event_id,
                3,
                duplicate_session_id,
                new_source_id,
                1,
                "event-1",
                "provider-source:new-source:1:event-1",
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms,
                 capture_source_id, payload_json, dedupe_key, fidelity, metadata_json)
                VALUES (?1, ?2, ?3, 'message', 'assistant', ?2, ?4, '{}', ?7,
                        'imported', json_object(
                            'provider_event_index', ?5,
                            'provider_event_hash', ?6
                        ))
                "#,
                params![
                    id.to_string(),
                    seq,
                    session_id.to_string(),
                    source_id.to_string(),
                    provider_index,
                    provider_hash,
                    dedupe_key,
                ],
            )
            .unwrap();
        }
        conn.execute(
            r#"
            INSERT INTO files_touched
            (id, event_id, path, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'src/lib.rs', 'explicit', 0, 0, 'imported')
            "#,
            params![file_touch_id.to_string(), duplicate_event_id.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 46;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2, "unexpected sessions: {sessions:?}");
    assert_eq!(
        store.get_session(old_session_id).unwrap().capture_source_id,
        Some(new_source_id)
    );
    assert_eq!(
        store.get_session(duplicate_session_id).unwrap().id,
        old_session_id
    );
    assert_eq!(
        store.get_event(duplicate_event_id).unwrap().id,
        old_event_id
    );
    assert_eq!(
        store.get_event(appended_event_id).unwrap().session_id,
        Some(old_session_id)
    );
    assert_eq!(store.events_for_session(old_session_id).unwrap().len(), 2);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT event_id FROM files_touched WHERE id = ?1",
                [file_touch_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        old_event_id.to_string()
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    let duplicate_insert = store.conn.execute(
        r#"
        INSERT INTO sessions
        (id, capture_source_id, provider, external_session_id, agent_type,
         is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
        VALUES (?1, ?2, 'claude', 'shared-provider-id', 'primary',
                1, 'imported', 'imported', 0, 4, 4)
        "#,
        params![new_id().to_string(), old_source_id.to_string()],
    );
    assert!(duplicate_insert
        .unwrap_err()
        .to_string()
        .contains("duplicate provider session"));
    assert_eq!(
        store
            .sessions_by_external_session_limited(
                CaptureProvider::Claude,
                "shared-provider-id",
                10,
            )
            .unwrap()
            .len(),
        2,
        "the different raw source path must remain distinct"
    );
    drop(store);

    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.list_sessions().unwrap().len(), 2);
    assert_eq!(
        reopened.get_session(duplicate_session_id).unwrap().id,
        old_session_id
    );
}
