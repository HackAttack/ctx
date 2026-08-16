use chrono::{DateTime, Utc};
use ctx_history_core::ProviderDeclaredFact;
use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ctx_history_capture_model::time::parse_rfc3339_utc;

use super::parser::{CursorSafePart, CursorSanitizedRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CursorNativeOrder {
    pub(crate) semantic_ordinal: u64,
    pub(crate) physical_ordinal: u64,
    pub(crate) part_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CursorEventBody {
    None,
    Text {
        text: String,
    },
    ToolCall {
        native_content: serde_json::Value,
        call_id: Option<String>,
        tool_name: Option<String>,
        arguments: Option<serde_json::Value>,
        protocol: Option<String>,
        server: Option<String>,
        explicit_tool: Option<String>,
        call_id_unavailable: bool,
        tool_name_unavailable: bool,
        arguments_unavailable: bool,
        mcp_identity_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ProviderDeclaredFact>,
    },
    ToolOutput {
        native_content: serde_json::Value,
        call_id: Option<String>,
        call_id_unavailable: bool,
        content_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ProviderDeclaredFact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorNativeEvent {
    pub(crate) native_order: CursorNativeOrder,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) body: CursorEventBody,
    pub(crate) record_byte_start: u64,
    pub(crate) record_byte_end_exclusive: u64,
    pub(crate) record_sha256: [u8; 32],
    pub(crate) provider_event_hash: [u8; 32],
}

pub(super) fn project_cursor_record(
    record: CursorSanitizedRecord,
) -> serde_json::Result<Vec<CursorNativeEvent>> {
    let occurred_at = record.timestamp.as_deref().and_then(parse_rfc3339_utc);
    record
        .parts
        .into_iter()
        .enumerate()
        .map(|(part_ordinal, part)| {
            let part_ordinal = u32::try_from(part_ordinal).unwrap_or(u32::MAX);
            let (event_type, role, body) = match part {
                CursorSafePart::BodyFree { event_type, role } => {
                    (event_type, role, CursorEventBody::None)
                }
                CursorSafePart::Text {
                    event_type,
                    role,
                    text,
                } => (event_type, role, CursorEventBody::Text { text }),
                CursorSafePart::ToolUse {
                    role,
                    native_content,
                    call_id,
                    tool_name,
                    arguments,
                    protocol,
                    server,
                    explicit_tool,
                    call_id_unavailable,
                    tool_name_unavailable,
                    arguments_unavailable,
                    mcp_identity_unavailable,
                    native_content_unavailable,
                    literal_facts,
                } => (
                    EventType::ToolCall,
                    role,
                    CursorEventBody::ToolCall {
                        native_content,
                        call_id,
                        tool_name,
                        arguments,
                        protocol,
                        server,
                        explicit_tool,
                        call_id_unavailable,
                        tool_name_unavailable,
                        arguments_unavailable,
                        mcp_identity_unavailable,
                        native_content_unavailable,
                        literal_facts,
                    },
                ),
                CursorSafePart::ToolResult {
                    role,
                    native_content,
                    call_id,
                    call_id_unavailable,
                    content_unavailable,
                    native_content_unavailable,
                    literal_facts,
                } => (
                    EventType::ToolOutput,
                    role,
                    CursorEventBody::ToolOutput {
                        native_content,
                        call_id,
                        call_id_unavailable,
                        content_unavailable,
                        native_content_unavailable,
                        literal_facts,
                    },
                ),
            };
            let provider_event_hash = cursor_logical_event_hash(
                event_type,
                role,
                occurred_at.map(|value| value.timestamp_millis()),
                &body,
            )?;
            Ok(CursorNativeEvent {
                native_order: CursorNativeOrder {
                    semantic_ordinal: record.semantic_ordinal,
                    physical_ordinal: record.physical_ordinal,
                    part_ordinal,
                },
                event_type,
                role,
                occurred_at,
                body,
                record_byte_start: record.byte_start,
                record_byte_end_exclusive: record.byte_end_exclusive,
                record_sha256: record.record_sha256,
                provider_event_hash,
            })
        })
        .collect()
}

fn cursor_logical_event_hash(
    event_type: EventType,
    role: EventRole,
    occurred_at_unix_ms: Option<i64>,
    body: &CursorEventBody,
) -> serde_json::Result<[u8; 32]> {
    let encoded = match body {
        CursorEventBody::None => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "none",
            serde_json::Value::Null,
        )),
        CursorEventBody::Text { text } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "text",
            text,
        )),
        CursorEventBody::ToolCall { native_content, .. } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "tool_call",
            native_content,
        )),
        CursorEventBody::ToolOutput { native_content, .. } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "tool_output",
            native_content,
        )),
    }?;
    Ok(Sha256::digest(encoded).into())
}
