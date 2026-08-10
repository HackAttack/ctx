use serde_json::Value;
use uuid::Uuid;

use super::{
    invalid_tool_request, optional_string, optional_usize, MCP_PRESENTATION_MAX_OUTPUT_BYTES,
};
use crate::tool_backend::{
    QueryEventFilters, QueryEventsRequest, ToolBackendError, ToolEventContent,
    ToolEventRangeDirection, ToolEventRangeScope, ToolOperation,
};

const DEFAULT_EVENT_QUERY_LIMIT: u64 = 10_000;

pub(super) fn query_events_operation(arguments: &Value) -> Result<ToolOperation, ToolBackendError> {
    let providers = optional_strings(arguments, "providers")?;
    let source = optional_string(arguments, "source")?;
    let history_source = optional_string(arguments, "history_source")?;
    let provider_key = optional_string(arguments, "provider_key")?;
    let source_id = optional_string(arguments, "source_id")?;
    let source_format = optional_string(arguments, "source_format")?;
    let provider_session = optional_string(arguments, "provider_session")?;
    let session = optional_string(arguments, "session")?;
    let parent_session = optional_string(arguments, "parent_session")?;
    let root_session = optional_string(arguments, "root_session")?;
    let branch = optional_string(arguments, "branch")?;
    let workspace = optional_string(arguments, "workspace")?;
    let event_type = optional_string(arguments, "event_type")?;
    let role = optional_string(arguments, "role")?;
    let agent_type = optional_string(arguments, "agent_type")?;
    let scope = optional_scope(arguments)?;
    let file = optional_string(arguments, "file")?;
    let direction = optional_direction(arguments)?;
    let filters = QueryEventFilters {
        providers,
        source_identity: parse_optional_uuid("source", source.as_deref())?,
        history_source,
        provider_key,
        source_id,
        source_format,
        provider_session_id: provider_session,
        session_id: parse_optional_uuid("session", session.as_deref())?,
        parent_session_id: parse_optional_uuid("parent_session", parent_session.as_deref())?,
        root_session_id: parse_optional_uuid("root_session", root_session.as_deref())?,
        branch,
        workspace,
        event_type,
        role,
        agent_type,
        scope,
        file,
        direction,
    };
    let since = optional_string(arguments, "since")?;
    let until = optional_string(arguments, "until")?;
    let cursor = optional_string(arguments, "cursor")?;
    let limit = optional_usize(arguments, "limit")?
        .map(usize_to_u64)
        .transpose()?
        .unwrap_or(DEFAULT_EVENT_QUERY_LIMIT);
    let content = optional_content_projection(arguments)?;
    Ok(ToolOperation::QueryEvents(QueryEventsRequest {
        since,
        until,
        filters,
        cursor,
        content,
        limit,
        output_limit_bytes: MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    }))
}

fn usize_to_u64(value: usize) -> Result<u64, ToolBackendError> {
    u64::try_from(value).map_err(|_| invalid_tool_request("numeric argument is too large"))
}

fn optional_strings(arguments: &Value, key: &str) -> Result<Vec<String>, ToolBackendError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid_tool_request(format!("{key} entries must be strings")))
            })
            .collect(),
        Some(_) => Err(invalid_tool_request(format!("{key} must be an array"))),
    }
}

fn parse_optional_uuid(
    key: &'static str,
    value: Option<&str>,
) -> Result<Option<Uuid>, ToolBackendError> {
    value
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| invalid_tool_request(format!("{key} must be a full UUID")))
        })
        .transpose()
}

fn optional_scope(arguments: &Value) -> Result<ToolEventRangeScope, ToolBackendError> {
    match optional_string(arguments, "scope")?.as_deref() {
        None | Some("all") => Ok(ToolEventRangeScope::All),
        Some("primary") => Ok(ToolEventRangeScope::Primary),
        Some("subagent") => Ok(ToolEventRangeScope::Subagent),
        Some(_) => Err(invalid_tool_request(
            "scope must be one of all, primary, subagent",
        )),
    }
}

fn optional_direction(arguments: &Value) -> Result<ToolEventRangeDirection, ToolBackendError> {
    match optional_string(arguments, "direction")?.as_deref() {
        None | Some("ascending") => Ok(ToolEventRangeDirection::Ascending),
        Some("descending") => Ok(ToolEventRangeDirection::Descending),
        Some(_) => Err(invalid_tool_request(
            "direction must be one of ascending, descending",
        )),
    }
}

fn optional_content_projection(arguments: &Value) -> Result<ToolEventContent, ToolBackendError> {
    match optional_string(arguments, "content")?.as_deref() {
        None | Some("full") => Ok(ToolEventContent::Full),
        Some("text") => Ok(ToolEventContent::Text),
        Some("none") => Ok(ToolEventContent::None),
        Some(_) => Err(invalid_tool_request(
            "content must be one of full, text, none",
        )),
    }
}
