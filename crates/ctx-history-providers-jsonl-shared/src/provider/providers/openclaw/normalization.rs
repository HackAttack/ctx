use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use ctx_history_capture_model::normalization::provider_role;

pub(crate) struct OpenClawEventFact {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) lexical_text: String,
}

pub(crate) fn event_fact(
    event_index: u64,
    _line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> OpenClawEventFact {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(openclaw_authored_text)
        .unwrap_or_default();
    OpenClawEventFact {
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        lexical_text: text,
    }
}

/// Selects only literal text authored into an OpenClaw conversation event.
///
/// Structural blocks stay available through the record's exact structured
/// content. In particular, tool names and tool-result shapes are not rendered
/// into synthetic transcript prose.
fn openclaw_authored_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let parts = blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .or_else(|| block.get("content"))
                        .or_else(|| block.get("output"))
                        .or_else(|| block.get("summary"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::event_fact;

    #[test]
    fn tool_only_blocks_do_not_invent_conversation_text() {
        for content in [
            json!([{
                "type": "toolCall",
                "id": "call-1",
                "name": "read_file",
                "arguments": {"path": "notes.txt"}
            }]),
            json!([{
                "type": "tool_result",
                "toolCallId": "call-1",
                "result": {"bytes": 12, "ok": true}
            }]),
        ] {
            let row = json!({
                "type": "message",
                "message": {"role": "assistant", "content": content}
            });
            let fact = event_fact(0, 1, &row, DateTime::<Utc>::UNIX_EPOCH);
            assert_eq!(fact.lexical_text, "");
        }
    }

    #[test]
    fn authored_text_is_exact_and_structural_blocks_are_ignored() {
        let row = json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "toolCall", "name": "must-not-appear"},
                    {"type": "text", "text": "second"}
                ]
            }
        });
        let fact = event_fact(0, 1, &row, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(fact.lexical_text, "first\nsecond");
    }
}
