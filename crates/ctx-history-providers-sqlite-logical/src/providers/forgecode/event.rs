use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::{compute_payload_hash, FORGECODE_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};
use ctx_history_capture_model::normalization::{
    provider_capped_json_value, provider_normalized_result_value, provider_role,
    provider_timestamp_value, provider_value_text,
};
#[derive(Debug, Clone, Copy)]
pub(super) struct ForgeCodeMessageParts<'a> {
    pub(super) variant: &'static str,
    pub(super) body: &'a Value,
    pub(super) usage: Option<&'a Value>,
}

pub(super) fn forgecode_message_parts(entry: &Value) -> ForgeCodeMessageParts<'_> {
    let message = entry.get("message").unwrap_or(entry);
    let usage = entry.get("usage");
    if let Some((variant, body)) = forgecode_message_variant(message) {
        return ForgeCodeMessageParts {
            variant,
            body,
            usage,
        };
    }
    ForgeCodeMessageParts {
        variant: "unknown",
        body: message,
        usage,
    }
}

fn forgecode_message_variant(value: &Value) -> Option<(&'static str, &Value)> {
    let Value::Object(object) = value else {
        return None;
    };
    for key in ["text", "tool", "image"] {
        if let Some(value) = object.get(key) {
            return Some((key, value));
        }
    }
    None
}

pub(super) fn forgecode_event_type(parts: ForgeCodeMessageParts<'_>) -> EventType {
    match parts.variant {
        "text" if forgecode_text_has_tool_calls(parts.body) => EventType::ToolCall,
        "text" => EventType::Message,
        "tool" => EventType::ToolOutput,
        "image" => EventType::Artifact,
        _ => EventType::Notice,
    }
}

pub(super) fn forgecode_event_role(parts: ForgeCodeMessageParts<'_>) -> Option<EventRole> {
    match parts.variant {
        "text" => forgecode_role_text(parts).map(|role| provider_role(Some(&role))),
        "tool" => Some(EventRole::Tool),
        "image" => Some(EventRole::Unknown),
        _ => None,
    }
}

pub(super) fn forgecode_role_text(parts: ForgeCodeMessageParts<'_>) -> Option<String> {
    forgecode_text_body(parts)
        .and_then(|body| body.get("role"))
        .and_then(Value::as_str)
        .map(|role| role.to_ascii_lowercase())
}

pub(super) fn forgecode_text_body(parts: ForgeCodeMessageParts<'_>) -> Option<&Value> {
    (parts.variant == "text").then_some(parts.body)
}

fn forgecode_text_has_tool_calls(body: &Value) -> bool {
    body.get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
}

pub(super) fn forgecode_message_text(
    parts: ForgeCodeMessageParts<'_>,
    event_type: EventType,
) -> String {
    match parts.variant {
        "text" => forgecode_text_message_text(parts.body, event_type),
        "tool" => forgecode_tool_result_text(parts.body),
        "image" => forgecode_image_text(parts.body),
        _ => provider_value_text(parts.body).unwrap_or_default(),
    }
}

pub(super) fn forgecode_event(
    provider_session_id: &str,
    entry: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ForgeCodeNativeEvent {
    let parts = forgecode_message_parts(entry);
    let event_type = forgecode_event_type(parts);
    let text = forgecode_message_text(parts, event_type);
    let body = json!({
        "message_index": provider_event_index,
        "message_variant": parts.variant,
        "message": entry,
        "usage": parts.usage,
    });
    ForgeCodeNativeEvent {
        provider_event_index,
        provider_event_hash: compute_payload_hash(entry).ok(),
        cursor: format!("conversation:{provider_session_id}:message:{provider_event_index}"),
        event_type,
        role: forgecode_event_role(parts),
        occurred_at,
        payload: json!({
            "text": text,
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "body": body,
        }),
        metadata: json!({
            "source": "forgecode_conversations",
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "conversation_id": provider_session_id,
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "role": forgecode_role_text(parts),
            "model": forgecode_text_body(parts)
                .and_then(|body| body.get("model"))
                .and_then(provider_value_text),
            "usage": parts.usage
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
        }),
    }
}

#[derive(Debug)]
pub(super) struct ForgeCodeNativeEvent {
    // Keep the native sequence with the event for non-Core materializers.
    #[allow(dead_code)]
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: Option<String>,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

pub(super) fn forgecode_text_message_text(body: &Value, _event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = body
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(content.to_owned());
    }
    if let Some(tool_text) = body
        .get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(forgecode_tool_calls_text)
    {
        parts.push(tool_text);
    }
    if parts.is_empty() {
        if let Some(raw_content) = body.get("raw_content").and_then(provider_value_text) {
            parts.push(raw_content);
        }
    }
    parts.join("\n")
}

fn forgecode_tool_calls_text(value: &Value) -> Option<String> {
    let calls = value.as_array()?;
    let mut parts = Vec::new();
    for call in calls {
        let name = call
            .get("name")
            .and_then(forgecode_scalar_text)
            .unwrap_or_else(|| "tool".to_owned());
        parts.push(format!("tool call: {name}"));
        if let Some(call_id) = call.get("call_id").and_then(forgecode_scalar_text) {
            parts.push(format!("tool call id: {call_id}"));
        }
        if let Some(arguments) = call
            .get("arguments")
            .and_then(provider_value_text)
            .filter(|text| !text.trim().is_empty())
        {
            parts.push(format!("tool input: {arguments}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn forgecode_tool_result_text(body: &Value) -> String {
    let name = body
        .get("name")
        .and_then(forgecode_scalar_text)
        .unwrap_or_else(|| "tool".to_owned());
    let mut parts = vec![format!("tool result: {name}")];
    if let Some(call_id) = body.get("call_id").and_then(forgecode_scalar_text) {
        parts.push(format!("tool call id: {call_id}"));
    }
    if let Some(content) = forgecode_normalized_result_content(body) {
        parts.push(content);
    }
    parts.join("\n")
}

/// Returns ForgeCode's complete normalized tool-result body.
///
/// The DTO owns an ordered `output.values` list. Variant selection below has
/// explicit precedence and never searches arbitrary descendants for an
/// output-looking field. The caller owns any byte bound.
pub(crate) fn forgecode_normalized_result_content(body: &Value) -> Option<String> {
    let values = body.pointer("/output/values").and_then(Value::as_array)?;
    let parts = values
        .iter()
        .filter_map(forgecode_tool_value_text)
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn forgecode_tool_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => {
            if let Some(child) = object_value_by_exact_variant_key(object, "text")
                .or_else(|| object_value_by_exact_variant_key(object, "markdown"))
            {
                return child.as_str().map(str::to_owned);
            }
            if let Some(child) = object_value_by_exact_variant_key(object, "ai") {
                return child
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| Some(provider_normalized_result_value(child)));
            }
            if let Some(child) = object_value_by_exact_variant_key(object, "image") {
                return Some(forgecode_image_text(child));
            }
            if let Some(child) = object_value_by_exact_variant_key(object, "filediff") {
                let path = child
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Some(format!("[File diff: {path}]"));
            }
            if let Some(items) =
                object_value_by_exact_variant_key(object, "pair").and_then(Value::as_array)
            {
                return items.first().and_then(forgecode_tool_value_text);
            }
            if object_value_by_exact_variant_key(object, "empty").is_some() {
                return None;
            }
            Some(provider_normalized_result_value(value))
        }
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(forgecode_tool_value_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

fn object_value_by_exact_variant_key<'a>(
    object: &'a serde_json::Map<String, Value>,
    expected: &str,
) -> Option<&'a Value> {
    object.get(expected).or_else(|| {
        let alias = match expected {
            "text" => "Text",
            "markdown" => "Markdown",
            "ai" => "Ai",
            "image" => "Image",
            "filediff" => "FileDiff",
            "pair" => "Pair",
            "empty" => "Empty",
            _ => return None,
        };
        object.get(alias)
    })
}

fn forgecode_image_text(body: &Value) -> String {
    let mime_type = body
        .get("mime_type")
        .or_else(|| body.get("mimeType"))
        .and_then(Value::as_str)
        .unwrap_or("image");
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty());
    match url {
        Some(url) => format!("ForgeCode image: {mime_type} {url}"),
        None => format!("ForgeCode image: {mime_type}"),
    }
}

fn forgecode_scalar_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| provider_value_text(value))
}

pub(super) fn forgecode_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f") {
        return DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc);
    }
    provider_timestamp_value(Some(&Value::String(raw.to_owned())), fallback)
}
