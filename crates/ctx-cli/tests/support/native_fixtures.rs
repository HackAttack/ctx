#![allow(unused_imports)]

use super::{fs, provider_history_fixture, Path, PathBuf, TempDir};
use serde_json::{json, Value};

mod appends;
mod installs;
mod json_tree;
mod sqlite;

pub(crate) use appends::*;
pub(crate) use installs::*;
pub(crate) use json_tree::*;
pub(crate) use sqlite::*;

pub(crate) fn write_native_grok_build_fixture(temp: &TempDir, query: &str) -> String {
    let source = PathBuf::from(provider_history_fixture(
        "grok-build/v1.0.3/sessions/synthetic-workspace/01990000-0000-7000-8000-000000000001/updates.jsonl",
    ));
    let destination = temp.path().join("grok-build").join("updates.jsonl");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let mut replaced = false;
    let mut output = String::new();
    for line in fs::read_to_string(source).unwrap().lines() {
        let mut record: Value = serde_json::from_str(line).unwrap();
        if record
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("agent_message_chunk")
        {
            *record
                .pointer_mut("/params/update/content")
                .expect("Grok fixture assistant content") = json!({"type": "text", "text": query});
            replaced = true;
        }
        output.push_str(&serde_json::to_string(&record).unwrap());
        output.push('\n');
    }
    assert!(replaced, "Grok fixture must contain an assistant message");
    fs::write(&destination, output).unwrap();
    destination.display().to_string()
}

pub(crate) fn clone_native_grok_build_session(
    source: &Path,
    destination: &Path,
    native_session_id: &str,
    query: &str,
) {
    let mut output = String::new();
    for (ordinal, line) in fs::read_to_string(source).unwrap().lines().enumerate() {
        let mut record: Value = serde_json::from_str(line).unwrap();
        *record
            .pointer_mut("/params/sessionId")
            .expect("Grok fixture session ID") = json!(native_session_id);
        if let Some(event_id) = record.pointer_mut("/params/_meta/eventId") {
            *event_id = json!(format!("{native_session_id}-{}", ordinal + 1));
        }
        if record
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("agent_message_chunk")
        {
            *record
                .pointer_mut("/params/update/content")
                .expect("Grok fixture assistant content") = json!({"type": "text", "text": query});
        }
        output.push_str(&serde_json::to_string(&record).unwrap());
        output.push('\n');
    }
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, output).unwrap();
}
