use serde_json::Value;

use super::{
    invalid_tool_request, optional_string, optional_transcript_mode, optional_usize,
    MAX_EVENT_WINDOW, MCP_DEFAULT_SESSION_PAGE_LIMIT, MCP_MAX_SESSION_CURSOR_BYTES,
    MCP_MAX_SESSION_PAGE_LIMIT, MCP_PRESENTATION_MAX_OUTPUT_BYTES,
};
use crate::tool_backend::{
    ShowEventRequest, ShowSessionRequest, ToolBackendError, ToolOperation, ToolTranscriptMode,
};

pub(super) fn show_session_operation(arguments: &Value) -> Result<ToolOperation, ToolBackendError> {
    let session_id = optional_string(arguments, "ctx_session_id")?
        .ok_or_else(|| invalid_tool_request("ctx_session_id is required"))?;
    validate_ctx_id(&session_id, "ctx_session_id", "session")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(ToolTranscriptMode::Lite);
    let limit = optional_usize(arguments, "limit")?.unwrap_or(MCP_DEFAULT_SESSION_PAGE_LIMIT);
    if !(1..=MCP_MAX_SESSION_PAGE_LIMIT).contains(&limit) {
        return Err(invalid_tool_request(format!(
            "limit must be between 1 and {MCP_MAX_SESSION_PAGE_LIMIT}"
        )));
    }
    let cursor = optional_session_cursor(arguments)?;
    Ok(ToolOperation::ShowSession(ShowSessionRequest {
        selector: session_id,
        mode,
        limit,
        cursor,
        output_limit_bytes: MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    }))
}

fn optional_session_cursor(arguments: &Value) -> Result<Option<String>, ToolBackendError> {
    let cursor = optional_string(arguments, "cursor")?;
    match cursor {
        Some(value)
            if value.is_empty()
                || value.len() > MCP_MAX_SESSION_CURSOR_BYTES
                || !value.is_ascii() =>
        {
            Err(invalid_tool_request(format!(
                "cursor must contain 1 to {MCP_MAX_SESSION_CURSOR_BYTES} ASCII bytes"
            )))
        }
        value => Ok(value),
    }
}

pub(super) fn show_event_operation(arguments: &Value) -> Result<ToolOperation, ToolBackendError> {
    let event_id = optional_string(arguments, "ctx_event_id")?
        .ok_or_else(|| invalid_tool_request("ctx_event_id is required"))?;
    validate_ctx_id(&event_id, "ctx_event_id", "event")?;
    let before = optional_usize(arguments, "before")?.unwrap_or(0);
    let after = optional_usize(arguments, "after")?.unwrap_or(0);
    let window = optional_usize(arguments, "window")?;
    if before > MAX_EVENT_WINDOW
        || after > MAX_EVENT_WINDOW
        || window.is_some_and(|window| window > MAX_EVENT_WINDOW)
    {
        return Err(invalid_tool_request(format!(
            "show_event before/after/window must be {MAX_EVENT_WINDOW} or less"
        )));
    }
    Ok(ToolOperation::ShowEvent(ShowEventRequest {
        selector: event_id,
        before,
        after,
        window,
        output_limit_bytes: MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    }))
}

fn validate_ctx_id(id: &str, argument: &str, kind: &str) -> Result<(), ToolBackendError> {
    if uuid::Uuid::parse_str(id.trim()).is_ok() {
        return Ok(());
    }
    normalize_uuid_prefix(id, kind)
        .map(|_| ())
        .map_err(|error| invalid_tool_request(format!("invalid {argument}: {error}")))
}

fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String, String> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(format!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        ));
    }
    if prefix.contains('-')
        || !prefix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ));
    }
    Ok(prefix.to_ascii_lowercase())
}
