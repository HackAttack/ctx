use std::fs;

use ctx_history_core::{AgentScope, EventType, ProviderNativeSessionRelationship};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{source::*, source_backed::*};

#[test]
fn file_catalog_projects_system_text_thinking_and_tool_activity() {
    let fixture = Fixture::new();
    fixture.write_index(json!({
        "version": 1,
        "sessions": {
            "session-a": {
                "sessionId": "session-a",
                "model": "claude-sonnet",
                "provider": "anthropic",
                "cwd": "/work/project",
                "workspaceRoot": "/work"
            }
        }
    }));
    fixture.write_manifest(
        "session-a",
        json!({
            "version": 1,
            "session_id": "session-a",
            "source": "cline",
            "messages_path": "session-a.messages.json"
        }),
    );
    let messages = messages_document(
        "session-a",
        vec![
            json!({
                "id": "m-user",
                "role": "user",
                "ts": 1_750_000_000_000_i64,
                "content": [{"type": "text", "text": "hello"}]
            }),
            json!({
                "id": "m-assistant",
                "role": "assistant",
                "modelInfo": {"id": "claude-sonnet", "provider": "anthropic"},
                "content": [
                    {"type": "thinking", "thinking": "considering"},
                    {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "README.md"}}
                ]
            }),
            json!({
                "id": "m-result",
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "call-1", "content": "contents", "is_error": false}]
            }),
        ],
    );
    fixture.write_messages("session-a", &messages);

    let tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_eq!(tree.leaves.len(), 1);
    let records = test_project_leaf(&tree.leaves[0].provider_leaf, &encoded(&messages)).unwrap();
    assert_eq!(records.len(), 5);
    assert_eq!(records[0].event_type, EventType::Notice.as_str());
    assert_eq!(records[0].role.as_deref(), Some("system"));
    assert_eq!(records[1].content.meaningful_text(), "hello");
    assert_eq!(records[2].content.meaningful_text(), "considering");
    assert_eq!(records[3].event_type, EventType::ToolCall.as_str());
    assert_eq!(records[4].event_type, EventType::ToolOutput.as_str());
    assert_eq!(
        records[3]
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.provider_call_id.as_ref()),
        records[4]
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.provider_call_id.as_ref())
    );
    assert_eq!(
        records[1]
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/session/model"))
            .and_then(Value::as_str),
        Some("claude-sonnet")
    );
}

#[test]
fn active_wal_database_only_reference_supplies_lineage_and_metadata() {
    let fixture = Fixture::new();
    let messages = messages_document(
        "child-session",
        vec![json!({
            "id": "message-1",
            "role": "assistant",
            "content": [{"type": "text", "text": "from the child"}]
        })],
    );
    fixture.write_messages("child-session", &messages);
    let connection = fixture.open_wal_database();
    insert_database_session(
        &connection,
        "child-session",
        Some("parent-session"),
        Some("agent-parent"),
        Some("agent-child"),
        "db-model",
        "/db/cwd",
        fixture.messages_path("child-session").to_str().unwrap(),
        true,
    );

    let tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_eq!(tree.leaves.len(), 1);
    let records = test_project_leaf(&tree.leaves[0].provider_leaf, &encoded(&messages)).unwrap();
    assert_eq!(records.len(), 2); // system prompt plus assistant text
    let record = records.last().unwrap();
    assert!(record.parent_session_id.is_some());
    assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(
        record.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(
        record
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/session/model"))
            .and_then(Value::as_str),
        Some("db-model")
    );
    drop(connection);
}

#[test]
fn duplicate_index_and_database_identity_is_one_source_with_database_precedence() {
    let fixture = Fixture::new();
    fixture.write_index(json!({
        "version": 1,
        "sessions": {
            "same-id": {
                "sessionId": "same-id",
                "model": "index-model",
                "cwd": "/index/cwd"
            }
        }
    }));
    let messages = messages_document(
        "same-id",
        vec![json!({"id": "m1", "role": "user", "content": "deduplicated"})],
    );
    fixture.write_messages("same-id", &messages);
    let connection = fixture.open_database();
    insert_database_session(
        &connection,
        "same-id",
        None,
        None,
        None,
        "database-model",
        "/database/cwd",
        fixture.messages_path("same-id").to_str().unwrap(),
        false,
    );
    drop(connection);

    let tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_eq!(tree.leaves.len(), 1);
    let records = test_project_leaf(&tree.leaves[0].provider_leaf, &encoded(&messages)).unwrap();
    let record = records.last().unwrap();
    assert_eq!(record.agent_scope, Some(AgentScope::Primary));
    assert_eq!(
        record
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/session/model"))
            .and_then(Value::as_str),
        Some("database-model")
    );
}

#[test]
fn append_rewrite_catalog_rewrite_and_delete_have_stable_lifecycle_identity() {
    let fixture = Fixture::new();
    fixture.write_index(single_index("life", "model-one"));
    let first = messages_document(
        "life",
        vec![json!({"id": "m1", "role": "user", "content": "one"})],
    );
    fixture.write_messages("life", &first);
    let cold_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    let cold_fingerprint = cold_tree.tree_fingerprint;
    let cold = test_project_leaf(&cold_tree.leaves[0].provider_leaf, &encoded(&first)).unwrap();
    let first_event_id = cold.last().unwrap().event_id;

    let appended = messages_document(
        "life",
        vec![
            json!({"id": "m1", "role": "user", "content": "one"}),
            json!({"id": "m2", "role": "assistant", "content": "two"}),
        ],
    );
    fixture.write_messages("life", &appended);
    let append_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_ne!(append_tree.tree_fingerprint, cold_fingerprint);
    let append =
        test_project_leaf(&append_tree.leaves[0].provider_leaf, &encoded(&appended)).unwrap();
    assert_eq!(append.last().unwrap().content.meaningful_text(), "two");
    assert_eq!(append[1].event_id, first_event_id);

    let rewritten = messages_document(
        "life",
        vec![
            json!({"id": "m1", "role": "user", "content": "one revised"}),
            json!({"id": "m2", "role": "assistant", "content": "two"}),
        ],
    );
    fixture.write_messages("life", &rewritten);
    let rewrite_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    let rewrite =
        test_project_leaf(&rewrite_tree.leaves[0].provider_leaf, &encoded(&rewritten)).unwrap();
    assert_eq!(rewrite[1].event_id, first_event_id);
    assert_eq!(rewrite[1].content.meaningful_text(), "one revised");

    fixture.write_index(single_index("life", "model-two"));
    let catalog_rewrite = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_ne!(
        catalog_rewrite.tree_fingerprint,
        rewrite_tree.tree_fingerprint
    );

    fixture.write_index(json!({"version": 1, "sessions": {}}));
    let deleted = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert!(deleted.leaves.is_empty());
}

#[test]
fn malformed_messages_are_leaf_local_and_recover_after_rewrite() {
    let fixture = Fixture::new();
    fixture.write_index(single_index("malformed", "model"));
    let path = fixture.messages_path("malformed");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{not-json").unwrap();

    let malformed_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_eq!(malformed_tree.leaves.len(), 1);
    assert!(test_project_leaf(&malformed_tree.leaves[0].provider_leaf, b"{not-json").is_err());

    let repaired = messages_document(
        "malformed",
        vec![json!({"id": "m1", "role": "user", "content": "repaired"})],
    );
    fixture.write_messages("malformed", &repaired);
    let repaired_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    assert_ne!(
        repaired_tree.tree_fingerprint,
        malformed_tree.tree_fingerprint
    );
    assert!(test_project_leaf(&repaired_tree.leaves[0].provider_leaf, &encoded(&repaired)).is_ok());
}

#[cfg(unix)]
#[test]
fn symlinked_message_artifact_is_rejected_without_following() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.write_index(single_index("linked", "model"));
    let outside = fixture.temp.path().join("outside.messages.json");
    fs::write(&outside, encoded(&messages_document("linked", Vec::new()))).unwrap();
    let path = fixture.messages_path("linked");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(&outside, &path).unwrap();

    assert!(test_bind_tree(&fixture.provider_root, &fixture.ctx_root).is_err());
}

#[test]
fn missing_message_ids_use_content_occurrence_fallback_not_global_position() {
    let fixture = Fixture::new();
    fixture.write_index(single_index("fallback", "model"));
    let first = messages_document(
        "fallback",
        vec![
            json!({"role": "user", "content": "same"}),
            json!({"role": "assistant", "content": "tail"}),
        ],
    );
    fixture.write_messages("fallback", &first);
    let first_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    let first_records =
        test_project_leaf(&first_tree.leaves[0].provider_leaf, &encoded(&first)).unwrap();
    let tail_id = first_records.last().unwrap().event_id;

    let inserted = messages_document(
        "fallback",
        vec![
            json!({"role": "system", "content": "unrelated"}),
            json!({"role": "user", "content": "same"}),
            json!({"role": "assistant", "content": "tail"}),
        ],
    );
    fixture.write_messages("fallback", &inserted);
    let inserted_tree = test_bind_tree(&fixture.provider_root, &fixture.ctx_root).unwrap();
    let inserted_records =
        test_project_leaf(&inserted_tree.leaves[0].provider_leaf, &encoded(&inserted)).unwrap();
    assert_eq!(inserted_records.last().unwrap().event_id, tail_id);
}

struct Fixture {
    temp: tempfile::TempDir,
    provider_root: std::path::PathBuf,
    ctx_root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let provider_root = temp.path().join("cline-data");
        let ctx_root = temp.path().join("ctx-data");
        fs::create_dir_all(provider_root.join("sessions")).unwrap();
        fs::create_dir_all(provider_root.join("db")).unwrap();
        fs::create_dir_all(&ctx_root).unwrap();
        Self {
            temp,
            provider_root,
            ctx_root,
        }
    }

    fn write_index(&self, value: Value) {
        fs::write(self.provider_root.join(INDEX_PATH), encoded(&value)).unwrap();
    }

    fn write_manifest(&self, session_id: &str, value: Value) {
        let path = self
            .provider_root
            .join("sessions")
            .join(session_id)
            .join(format!("{session_id}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, encoded(&value)).unwrap();
    }

    fn messages_path(&self, session_id: &str) -> std::path::PathBuf {
        self.provider_root
            .join("sessions")
            .join(session_id)
            .join(format!("{session_id}.messages.json"))
    }

    fn write_messages(&self, session_id: &str, value: &Value) {
        let path = self.messages_path(session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, encoded(value)).unwrap();
    }

    fn open_database(&self) -> Connection {
        let connection = Connection::open(self.provider_root.join(DATABASE_PATH)).unwrap();
        create_database_schema(&connection);
        connection
    }

    fn open_wal_database(&self) -> Connection {
        let connection = Connection::open(self.provider_root.join(DATABASE_PATH)).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        create_database_schema(&connection);
        connection
    }
}

fn create_database_schema(connection: &Connection) {
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                source TEXT,
                started_at INTEGER,
                updated_at INTEGER,
                provider TEXT,
                model TEXT,
                cwd TEXT,
                workspace_root TEXT,
                parent_session_id TEXT,
                parent_agent_id TEXT,
                agent_id TEXT,
                conversation_id TEXT,
                is_subagent INTEGER,
                metadata_json TEXT,
                messages_path TEXT
            );",
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_database_session(
    connection: &Connection,
    session_id: &str,
    parent_session_id: Option<&str>,
    parent_agent_id: Option<&str>,
    agent_id: Option<&str>,
    model: &str,
    cwd: &str,
    messages_path: &str,
    is_subagent: bool,
) {
    connection
        .execute(
            "INSERT INTO sessions (
                session_id, source, started_at, updated_at, provider, model, cwd,
                workspace_root, parent_session_id, parent_agent_id, agent_id,
                conversation_id, is_subagent, metadata_json, messages_path
            ) VALUES (?1, 'cline', 1740000000000, 1750000000000, 'anthropic', ?2, ?3,
                '/workspace', ?4, ?5, ?6, 'conversation-1', ?7, '{}', ?8)",
            params![
                session_id,
                model,
                cwd,
                parent_session_id,
                parent_agent_id,
                agent_id,
                i64::from(is_subagent),
                messages_path,
            ],
        )
        .unwrap();
}

fn single_index(session_id: &str, model: &str) -> Value {
    json!({
        "version": 1,
        "sessions": {
            (session_id): {
                "sessionId": session_id,
                "model": model,
                "cwd": "/fixture/cwd"
            }
        }
    })
}

fn messages_document(session_id: &str, messages: Vec<Value>) -> Value {
    json!({
        "version": 1,
        "updated_at": "2025-06-15T12:00:00Z",
        "agent": "lead",
        "sessionId": session_id,
        "system_prompt": "You are Cline.",
        "messages": messages
    })
}

fn encoded(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
