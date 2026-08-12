use std::borrow::Cow;

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::{OutputOutcome, OutputOutcomeMetadata};

use super::super::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};

fn update(value: &Value) -> &Value {
    value
        .pointer("/params/update")
        .or_else(|| value.get("update"))
        .unwrap_or(value)
}

fn envelope_meta(value: &Value) -> Option<&Value> {
    value
        .pointer("/params/_meta")
        .or_else(|| value.get("_meta"))
}

fn update_kind(value: &Value) -> Option<&str> {
    update(value).get("sessionUpdate").and_then(Value::as_str)
}

pub(crate) fn grok_build_header_session_id(value: &Value) -> Option<String> {
    value
        .pointer("/params/sessionId")
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn grok_build_event_identity(value: &Value) -> Option<&str> {
    envelope_meta(value)
        .and_then(|meta| meta.get("eventId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub(crate) fn grok_build_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    envelope_meta(value)
        .and_then(|meta| meta.get("agentTimestampMs"))
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        })
}

pub(crate) fn grok_build_event_type(value: &Value) -> EventType {
    match update_kind(value) {
        Some("user_message_chunk" | "agent_message_chunk") => EventType::Message,
        Some("agent_thought_chunk" | "plan" | "rewind_marker") => EventType::Summary,
        Some("tool_call") => EventType::ToolCall,
        Some("tool_call_update")
            if terminal_status(value).is_some()
                && grok_build_tool_kind(value) == Some("execute") =>
        {
            EventType::CommandOutput
        }
        Some("tool_call_update") if terminal_status(value).is_some() => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

pub(crate) fn grok_build_role(value: &Value) -> EventRole {
    match update_kind(value) {
        Some("user_message_chunk") => EventRole::User,
        Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call") => EventRole::Assistant,
        Some("tool_call_update") => EventRole::Tool,
        _ => EventRole::System,
    }
}

pub(crate) fn grok_build_event_text(value: &Value) -> String {
    let update = update(value);
    match update_kind(value) {
        Some("user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk") => update
            .get("content")
            .and_then(visible_text)
            .unwrap_or_default(),
        Some("tool_call") => grok_build_structured_tool_call_text(value).unwrap_or_default(),
        Some("plan" | "rewind_marker" | "tool_call_update") => update
            .get("content")
            .and_then(visible_text)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_default(),
        // New ACP update variants are intentionally contentless until their
        // shape has been audited. This prevents a future metadata payload from
        // silently becoming searchable user-visible history.
        Some(_) | None => String::new(),
    }
}

pub(crate) fn grok_build_structured_tool_call_text(value: &Value) -> Option<String> {
    let update = update(value);
    (update_kind(value) == Some("tool_call")).then(|| {
        json!({
            "toolCallId": update.get("toolCallId"),
            "title": update.get("title"),
            "rawInput": update.get("rawInput"),
        })
        .to_string()
    })
}

pub(crate) fn enumerate_grok_build_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if update_kind(value) != Some("tool_call_update") {
        return Ok(Vec::new());
    }
    let Some(status) = terminal_status(value) else {
        return Ok(Vec::new());
    };
    let update = update(value);
    let content = grok_build_result_content(update)
        .filter(|text| !text.trim().is_empty())
        .map(Cow::Owned);
    let call_id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content,
        call_id,
        tool_name: update.get("title").and_then(Value::as_str),
        outcome: OutputOutcomeMetadata {
            outcome: terminal_outcome(update, status),
            exit_code: terminal_exit_code(update),
            duration_ms: None,
        },
    }])
}

fn terminal_status(value: &Value) -> Option<&str> {
    update(value)
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "completed" | "failed" | "cancelled" | "timed_out"))
}

fn grok_build_tool_kind(value: &Value) -> Option<&str> {
    update(value)
        .pointer("/_meta/x.ai~1tool/kind")
        .and_then(Value::as_str)
        .or_else(|| update(value).get("kind").and_then(Value::as_str))
        .or_else(|| {
            (update(value)
                .pointer("/rawOutput/type")
                .and_then(Value::as_str)
                == Some("Bash"))
            .then_some("execute")
        })
}

fn terminal_exit_code(update: &Value) -> Option<i32> {
    update
        .pointer("/rawOutput/exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

fn terminal_outcome(update: &Value, status: &str) -> OutputOutcome {
    let raw_output = update.get("rawOutput");
    let timed_out = raw_output
        .and_then(|raw| raw.get("timed_out"))
        .and_then(Value::as_bool)
        == Some(true)
        || raw_output
            .and_then(|raw| raw.get("is_timeout"))
            .and_then(Value::as_bool)
            == Some(true)
        || status == "timed_out";
    if timed_out {
        return OutputOutcome::Timeout;
    }
    let raw_type = raw_output
        .and_then(|raw| raw.get("type"))
        .and_then(Value::as_str);
    let nonzero_bash_exit = raw_type == Some("Bash")
        && terminal_exit_code(update).is_some_and(|exit_code| exit_code != 0);
    let fatal_bash_signal = raw_type == Some("Bash")
        && raw_output
            .and_then(|raw| raw.get("signal"))
            .and_then(Value::as_str)
            .is_some_and(|signal| signal != "backgrounded");
    let explicit_error = raw_output
        .and_then(|raw| raw.get("is_error"))
        .and_then(Value::as_bool)
        == Some(true);
    if nonzero_bash_exit
        || fatal_bash_signal
        || explicit_error
        || matches!(status, "failed" | "cancelled")
    {
        OutputOutcome::Failure
    } else if status == "completed" {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    }
}

fn grok_build_result_content(update: &Value) -> Option<String> {
    if let Some(content) = update.get("content").filter(|content| !content.is_null()) {
        return visible_text(content)
            .or_else(|| serde_json::to_string(content).ok())
            .filter(|content| !content.trim().is_empty());
    }
    raw_output_visible_text(update.get("rawOutput")?)
}

fn raw_output_visible_text(raw_output: &Value) -> Option<String> {
    let output_type = raw_output.get("type").and_then(Value::as_str)?;
    let selected = match output_type {
        "ListDir" => [
            "/Content/content",
            "/NotFound",
            "/IsAFile",
            "/NotADirectory",
            "/PermissionDenied",
            "/Error",
        ]
        .into_iter()
        .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
        .map(str::to_owned),
        "WebSearch" => raw_output
            .get("pre_formatted")
            .and_then(Value::as_str)
            .or_else(|| raw_output.get("content").and_then(Value::as_str))
            .map(str::to_owned),
        "Todo" => [
            "/TodosUpdated/summary_for_prompt",
            "/DuplicateId",
            "/InvalidArgument",
        ]
        .into_iter()
        .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
        .map(str::to_owned),
        "MCP" => ["/output/OkayOutput", "/output/Error"]
            .into_iter()
            .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
            .map(str::to_owned),
        _ => None,
    };
    selected.filter(|content| !content.trim().is_empty())
}

fn visible_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().map(visible_text).collect::<Option<Vec<_>>>()?;
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| object.get("content").and_then(visible_text)),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_terminal_text_and_failure_are_retained() {
        let value = json!({
            "timestamp": 1,
            "method": "session/update",
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "failed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "failed safely"}}]
                },
                "_meta": {"eventId": "event", "agentTimestampMs": 1_700_000_000_000_i64}
            }
        });
        assert_eq!(grok_build_event_type(&value), EventType::ToolOutput);
        let results = enumerate_grok_build_results(&value).unwrap();
        assert_eq!(results[0].content.as_deref(), Some("failed safely"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Failure);
    }

    #[test]
    fn raw_output_is_not_used_as_ordinary_visible_content() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "rawOutput": {"absolute_path": "/private/source.rs"}
                }
            }
        });
        assert!(enumerate_grok_build_results(&value).unwrap()[0]
            .content
            .is_none());
    }

    #[test]
    fn execute_completion_is_command_output() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "ok"}}],
                    "_meta": {"x.ai/tool": {"kind": "execute"}}
                }
            }
        });
        assert_eq!(grok_build_event_type(&value), EventType::CommandOutput);
    }

    #[test]
    fn native_bash_timeout_and_nonzero_exit_override_status() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "failed",
                    "content": "timed out",
                    "rawOutput": {"type": "Bash", "timed_out": true, "exit_code": 0}
                }
            }
        });
        let results = enumerate_grok_build_results(&value).unwrap();
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Timeout);

        let nonzero = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "content": "command failed",
                    "rawOutput": {"type": "Bash", "timed_out": false, "exit_code": 17}
                }
            }
        });
        let results = enumerate_grok_build_results(&nonzero).unwrap();
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Failure);
        assert_eq!(results[0].outcome.exit_code, Some(17));
    }

    #[test]
    fn typed_diff_and_raw_output_only_results_are_retained() {
        let diff = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "diff-call",
                    "status": "completed",
                    "content": [{"type": "diff", "path": "/repo/lib.rs", "oldText": "old", "newText": "new"}],
                    "rawOutput": {"type": "SearchReplace"}
                }
            }
        });
        let diff_result = enumerate_grok_build_results(&diff).unwrap();
        assert!(diff_result[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("\"type\":\"diff\"")));

        let list = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "list-call",
                    "status": "completed",
                    "rawOutput": {"type": "ListDir", "Content": {"content": "src/\\nCargo.toml", "absolute_root_path": "/repo"}}
                }
            }
        });
        let list_result = enumerate_grok_build_results(&list).unwrap();
        assert_eq!(list_result[0].content.as_deref(), Some("src/\\nCargo.toml"));
    }

    #[test]
    fn timestamp_prefers_agent_milliseconds_and_falls_back_to_envelope_seconds() {
        let preferred = json!({
            "timestamp": 1,
            "params": {"_meta": {"agentTimestampMs": 1_700_000_000_123_i64}}
        });
        assert_eq!(
            grok_build_timestamp(&preferred).unwrap().timestamp_millis(),
            1_700_000_000_123
        );

        let fallback = json!({"timestamp": 1_700_000_001_i64});
        assert_eq!(
            grok_build_timestamp(&fallback).unwrap().timestamp_millis(),
            1_700_000_001_000
        );
    }

    #[test]
    fn unknown_future_update_does_not_retain_untrusted_body() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "future_update_v2",
                    "content": {"type": "text", "text": "future-sensitive-marker"},
                    "rawOutput": {"type": "FutureResult", "secret": "future-secret"}
                }
            }
        });
        assert_eq!(grok_build_event_type(&value), EventType::Notice);
        assert!(grok_build_event_text(&value).is_empty());
        assert!(enumerate_grok_build_results(&value).unwrap().is_empty());
    }
}
