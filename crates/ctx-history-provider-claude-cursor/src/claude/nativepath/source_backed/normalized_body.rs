use crate::claude::nativepath::rows::{ClaudeEventKind, ClaudeRetainedRow};

pub(super) fn lexical_body(row: &ClaudeRetainedRow) -> String {
    let text = row
        .body
        .clone()
        .or_else(|| {
            row.tool_call
                .as_ref()
                .and_then(|call| call.input.as_ref())
                .and_then(|input| serde_json::to_string(input).ok())
                .filter(|text| text.len() <= ctx_history_core::MAX_CORE_CONTENT_BYTES)
        })
        .or_else(|| {
            row.tool_result
                .as_ref()
                .and_then(|result| result.native_content.get("content"))
                .and_then(|content| match content {
                    serde_json::Value::String(value)
                        if value.len() <= ctx_history_core::MAX_CORE_CONTENT_BYTES =>
                    {
                        Some(value.clone())
                    }
                    _ => None,
                })
        })
        .unwrap_or_else(|| event_kind(row.kind).to_owned());
    if text.trim().is_empty() {
        event_kind(row.kind).to_owned()
    } else {
        text
    }
}

pub(super) fn event_kind(kind: ClaudeEventKind) -> &'static str {
    match kind {
        ClaudeEventKind::Message => "message",
        ClaudeEventKind::Summary => "summary",
        ClaudeEventKind::Notice => "notice",
        ClaudeEventKind::ToolCall => "tool_call",
        ClaudeEventKind::ToolOutput => "tool_output",
    }
}
