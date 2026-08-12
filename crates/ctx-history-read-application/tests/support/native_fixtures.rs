use rusqlite::{params, Connection};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

use crate::support::copy_dir_all;

pub(crate) fn write_pi_session_jsonl(path: &Path, id: &str, query: &str) {
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "version": 3,
                "id": id,
                "timestamp": "2026-06-24T12:00:00.000Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": format!("{id}-user"),
                "timestamp": "2026-06-24T12:00:01.000Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": query}]
                }
            })
        ),
    )
    .unwrap();
}

pub(crate) fn write_native_claude_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("claude-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "uuid": "claude-cli-native-user"
            }),
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:01Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]},
                "uuid": "claude-cli-native-assistant"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-claude/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_rovodev_fixture(temp: &TempDir, query: &str) -> String {
    let session = temp
        .path()
        .join("native-rovodev/sessions/rovodev-cli-native");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        json!({
            "session_id": "rovodev-cli-native",
            "title": "Rovo Dev CLI native",
            "workspace_path": "/workspace/rovodev",
            "created_at": "2026-07-04T18:20:00Z",
            "updated_at": "2026-07-04T18:20:02Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("session_context.json"),
        json!({
            "message_history": [
                {
                    "id": "rovodev-cli-native-user",
                    "role": "user",
                    "created_at": "2026-07-04T18:20:00Z",
                    "parts": [{"kind": "text", "text": query}]
                },
                {
                    "id": "rovodev-cli-native-assistant",
                    "role": "assistant",
                    "created_at": "2026-07-04T18:20:01Z",
                    "parts": [
                        {"kind": "text", "text": "rovodev native import ok"},
                        {"kind": "tool_use", "name": "Write", "input": {"path": "src/rovodev_cli_native.txt", "content": "proof"}}
                    ]
                },
                {
                    "id": "rovodev-cli-native-tool",
                    "role": "tool",
                    "created_at": "2026-07-04T18:20:02Z",
                    "parts": [{"kind": "tool_result", "content": "wrote src/rovodev_cli_native.txt"}]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    temp.path()
        .join("native-rovodev/sessions")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_junie_fixture(temp: &TempDir, query: &str) -> String {
    let sessions = temp.path().join("native-junie/sessions");
    let session_id = "session-260607-120000-native";
    let session = sessions.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1783348800000i64,
                "updatedAt": 1783348920000i64,
                "taskName": "Junie native CLI fixture",
                "projectDir": "/workspace/junie-native"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "kind": "UserPromptEvent",
                "prompt": query
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783348920000i64,
                "event": {
                    "agentEvent": {
                        "kind": "ResultBlockUpdatedEvent",
                        "stepId": "result-1",
                        "result": format!("Junie answered {query}")
                    }
                }
            })
        ),
    )
    .unwrap();
    sessions.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_cursor_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-cursor/projects/sanitized-workspace/agent-transcripts/cursor-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("cursor-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-06-24T12:00:00Z",
                "role": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]}
            }),
            json!({
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-cursor/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_qoder_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-qoder/projects/sanitized-workspace/transcript");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("qoder-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n{}\n",
            json!({
                "type": "session_meta",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-meta",
                "timestamp": "2026-07-01T12:00:00Z",
                "cwd": "/workspace/qoder-cli",
                "data": {
                    "meta_type": "session_info",
                    "content": {"mode": "agent", "session_type": "assistant"}
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-user",
                "timestamp": "2026-07-01T12:00:01Z",
                "cwd": "/workspace/qoder-cli",
                "message": {"role": "user", "content": query}
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-assistant",
                "timestamp": "2026-07-01T12:00:02Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "qoder native import ok"}]
                }
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool",
                "timestamp": "2026-07-01T12:00:03Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-qoder-cli-read",
                        "name": "read_file",
                        "input": {"file_path": "src/qoder_cli_native.py"}
                    }]
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool-result",
                "timestamp": "2026-07-01T12:00:04Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-qoder-cli-read",
                        "content": "qoderleakproofxylophonium",
                        "is_error": false
                    }]
                },
                "toolUseResult": {
                    "content": "qoderleakproofxylophonium",
                    "callId": "call-qoder-cli-read",
                    "toolName": "read_file",
                    "exitCode": 0
                }
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-qoder/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_openhands_fixture(temp: &TempDir, query: &str) -> String {
    let conversation = temp
        .path()
        .join("native-openhands/local-user/v1_conversations/12345678123456781234567812345678");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
        serde_json::to_string_pretty(&json!({
            "id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "timestamp": "2026-06-24T12:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": [{"type": "text", "text": query}]
            },
            "activated_microagents": [],
            "extended_content": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.json"),
        serde_json::to_string_pretty(&json!({
            "id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "timestamp": "2026-06-24T12:00:01Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "str_replace",
                "path": "openhands-cli-native-oracle.txt",
                "file_text": null,
                "old_str": "old",
                "new_str": "new",
                "insert_line": null,
                "view_range": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "tool_call": {
                "id": "call-openhands-file",
                "type": "function",
                "function": {
                    "name": "FileEditor",
                    "arguments": "{\"command\":\"str_replace\"}"
                }
            },
            "llm_response_id": "response-openhands-file",
            "security_risk": "LOW",
            "thought": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("cccccccccccccccccccccccccccccccc.json"),
        serde_json::to_string_pretty(&json!({
            "id": "cccccccccccccccccccccccccccccccc",
            "timestamp": "2026-06-24T12:00:02Z",
            "source": "environment",
            "observation": {
                "kind": "FileEditorObservation",
                "command": "str_replace",
                "output": "openhandssuccesstooloutputsentinel",
                "path": "openhands-cli-native-oracle.txt",
                "prev_exist": true,
                "old_content": "old",
                "new_content": "new",
                "error": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "action_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }))
        .unwrap(),
    )
    .unwrap();
    temp.path()
        .join("native-openhands")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_continue_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-continue/sessions");
    fs::create_dir_all(&root).unwrap();
    let session_id = "continue-cli-native";
    fs::write(
        root.join("sessions.json"),
        serde_json::to_string_pretty(&json!([
            {
                "sessionId": session_id,
                "title": "native continue",
                "dateCreated": "2026-06-24T12:00:00Z",
                "workspaceDirectory": "/workspace",
                "messageCount": 1
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&json!({
            "sessionId": session_id,
            "title": "native continue",
            "workspaceDirectory": "/workspace",
            "history": [
                {
                    "id": "continue-cli-native-user",
                    "timestamp": "2026-06-24T12:00:01Z",
                    "message": {
                        "role": "user",
                        "content": query
                    },
                    "contextItems": [
                        {
                            "name": "fixture.rs",
                            "content": "Continue context item marker"
                        }
                    ],
                    "editorState": query
                },
                {
                    "id": "continue-cli-native-assistant",
                    "timestamp": "2026-06-24T12:00:02Z",
                    "message": {
                        "role": "assistant",
                        "content": "native Continue import ok"
                    },
                    "toolCallStates": [
                        {
                            "toolCallId": "tool-continue-read",
                            "toolCall": {
                                "id": "tool-continue-read",
                                "type": "function",
                                "function": {
                                    "name": "readFile",
                                    "arguments": "{\"filepath\":\"fixture.rs\"}"
                                }
                            },
                            "status": "done",
                            "output": [
                                {
                                    "name": "Result",
                                    "description": "",
                                    "content": "continuesuccesstooloutputsentinel"
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {
                "totalCost": 0,
                "promptTokens": 12,
                "completionTokens": 8
            }
        }))
        .unwrap(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

/// Writes a Kimi Code CLI wire tree in the shape the CLI actually journals:
/// the assistant reply exists only as streamed `content.part` loop events.

pub(crate) fn write_native_kilo_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-kilo.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key,
            project_id text not null,
            parent_id text,
            slug text not null,
            directory text not null,
            title text not null,
            version text not null,
            model text,
            agent text,
            cost real not null default 0,
            tokens_input integer not null default 0,
            tokens_output integer not null default 0,
            tokens_reasoning integer not null default 0,
            tokens_cache_read integer not null default 0,
            tokens_cache_write integer not null default 0,
            time_created integer not null,
            time_updated integer not null
        );
        create table session_message (
            id text primary key,
            session_id text not null,
            type text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table part (
            id text primary key,
            message_id text not null,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table todo (
            session_id text not null,
            content text not null,
            status text not null,
            priority text not null,
            position integer not null,
            time_created integer not null,
            time_updated integer not null
        );
        create table permission (
            project_id text primary key,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, model, agent,
            time_created, time_updated
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            '{\"id\":\"kilo-auto/free\",\"providerID\":\"kilo\"}', 'build',
            1782259200000, 1782259200000)",
        ["kilo-cli-native"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "kilo-cli-native-user",
            "kilo-cli-native",
            &format!(r#"{{"time":{{"created":1782259200000}},"text":"{query}"}}"#),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_lingma_sqlite_fixture(path: &Path, query: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE chat_record (
            session_id TEXT NOT NULL,
            request_id TEXT,
            chat_prompt TEXT,
            summary TEXT,
            error_result TEXT,
            gmt_create INTEGER,
            extra TEXT
        );
        "#,
    )
    .unwrap();
    conn.execute(
        r#"
        INSERT INTO chat_record
            (session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            "lingma-cli-session",
            "lingma-cli-request",
            query,
            "Lingma CLI assistant summary import ok",
            "{}",
            1_783_166_400_000_i64,
            json!({"model": "lingma-cli-fixture"}).to_string(),
        ],
    )
    .unwrap();
}

pub(crate) fn write_native_astrbot_fixture(temp: &TempDir, query: &str) -> String {
    let data = temp.path().join("native-astrbot/data");
    fs::create_dir_all(&data).unwrap();
    let path = data.join("data_v4.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            id integer primary key,
            inner_conversation_id text,
            conversation_id text,
            platform_id text,
            user_id text,
            content text not null,
            title text,
            persona_id text,
            token_usage text,
            created_at integer,
            updated_at integer
        );
        create table preferences (
            scope text,
            key text,
            value text
        );
        create table platform_message_history (
            id integer primary key,
            platform_id text,
            user_id text,
            sender_id text,
            sender_name text,
            content text,
            llm_checkpoint_id text,
            created_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            1, 'umo-1', 'conv-1', 'webchat', 'user-1', ?1, 'native astrbot',
            'default', ?2, 1782259200000, 1782259202000
        )",
        [
            json!([
                {"role": "user", "content": query},
                {"type": "_checkpoint", "id": "checkpoint-1"},
                {"role": "assistant", "content": "native import ok"}
            ])
            .to_string(),
            json!({"prompt": 1, "completion": 1}).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into preferences values ('umo', 'sel_conv_id', 'conv-1')",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into platform_message_history values (
            1, 'webchat', 'user-1', 'user-1', 'User', ?1, 'checkpoint-1', 1782259201000
        )",
        [json!({"text": query}).to_string()],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn install_default_claude_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_claude_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".claude").join("projects"));
}

pub(crate) fn install_default_pi_fixture(temp: &TempDir, query: &str) {
    let root = temp.path().join(".pi/agent/sessions/--workspace--");
    fs::create_dir_all(&root).unwrap();
    write_pi_session_jsonl(
        &root.join("2026-06-24T12-00-00-000Z_pi-default-refresh.jsonl"),
        "pi-default-refresh",
        query,
    );
}

pub(crate) fn install_default_cursor_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_cursor_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".cursor").join("projects"));
}

pub(crate) fn install_default_qoder_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_qoder_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".qoder").join("projects"));
}

pub(crate) fn install_default_kilo_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_kilo_fixture(temp, query));
    let target = temp.path().join(".local/share/kilo");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("kilo.db")).unwrap();
}

pub(crate) fn install_default_astrbot_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_astrbot_fixture(temp, query));
    let target = temp.path().join(".astrbot/data");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("data_v4.db")).unwrap();
}

pub(crate) fn install_default_continue_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_continue_fixture(temp, query));
    let target = temp.path().join(".continue").join("sessions");
    fs::create_dir_all(&target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
        }
    }
}

pub(crate) fn install_default_rovodev_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_rovodev_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".rovodev").join("sessions"));
}

pub(crate) fn install_default_lingma_fixture(temp: &TempDir, query: &str) {
    let target = temp
        .path()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    write_lingma_sqlite_fixture(&target, query);
}

pub(crate) fn install_default_junie_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_junie_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".junie").join("sessions"));
}

pub(crate) fn install_default_openhands_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_openhands_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".openhands"));
}
