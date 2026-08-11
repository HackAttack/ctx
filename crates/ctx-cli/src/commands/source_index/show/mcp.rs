use std::path::Path;

use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::CoreEventPageBudget;
use ctx_history_read_application::{
    EventWindowBudget, PinnedHistoryQuery, ShowEventRequest, ShowSessionPageRequest,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{output::OutputFormat, transcript::TranscriptMode};

use super::{
    event_window_json_with_lineage, paginated_session_transcript_value, session_event_mode,
    ShowApplicationError, ShowApplicationResult, CLI_SESSION_EVENT_PAGE_ITEMS,
};
use crate::commands::source_index::{
    compact_presentation::CompactPresentation,
    render::enforce_json_output_limit,
    shared::{externalize_query_error, index_root, open_index},
};

#[cfg(test)]
pub(crate) fn mcp_show_session(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> anyhow::Result<Value> {
    mcp_show_session_with_compact(data_root, id, mode, limit, cursor, output_limit_bytes)
        .map(|(value, _)| value)
}

#[cfg(test)]
pub(crate) fn mcp_show_session_with_compact(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> anyhow::Result<(Value, Value)> {
    mcp_show_session_application(data_root, id, mode, limit, cursor, output_limit_bytes)
        .map_err(ShowApplicationError::into_cli_error)
}

pub(crate) fn mcp_show_session_application(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> ShowApplicationResult<(Value, Value)> {
    let index = open_index(data_root).map_err(ShowApplicationError::from_application_error)?;
    let compact = CompactPresentation::open(&index, &index_root(data_root))
        .map_err(externalize_query_error)
        .map_err(ShowApplicationError::from_application_error)?;
    let cursor = cursor
        .map(ctx_history_read_application::decode_session_event_cursor)
        .transpose()
        .map_err(ShowApplicationError::from_application_error)?;
    let query = PinnedHistoryQuery::new(&index, compact.retained_peer());
    let page = query
        .show_session_page(&ShowSessionPageRequest {
            selector: Some(id.to_owned()),
            provider_session_id: None,
            provider: None,
            mode: session_event_mode(mode),
            cursor,
            limit,
            page_items: CLI_SESSION_EVENT_PAGE_ITEMS,
            page_budget: CoreEventPageBudget::new(
                output_limit_bytes.clamp(1, MAX_ENCODED_CORE_RECORD_BYTES),
                output_limit_bytes.clamp(1, MAX_CORE_CONTENT_BYTES),
            ),
        })
        .map_err(externalize_query_error)
        .map_err(ShowApplicationError::from_application_error)?;
    let session = page.session;
    let rendered = ctx_history_read_application::retain_structured_session_page(
        page.events,
        page.has_more,
        output_limit_bytes,
    )
    .map_err(ShowApplicationError::from_application_error)?;
    let value = paginated_session_transcript_value(
        &session,
        mode,
        OutputFormat::Json,
        rendered.events,
        limit,
        rendered.has_more,
        rendered.next_cursor.as_ref(),
    )
    .map_err(ShowApplicationError::from_application_error)?;
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(|| session.session_id.as_uuid());
    enforce_json_output_limit(&value, output_limit_bytes, event_id)
        .map_err(ShowApplicationError::from_application_error)?;
    let compact_value = compact
        .project(&value)
        .map_err(ShowApplicationError::from_application_error)?;
    Ok((value, compact_value))
}

#[cfg(test)]
pub(crate) fn mcp_show_event(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> anyhow::Result<Value> {
    mcp_show_event_with_compact(data_root, id, before, after, window, output_limit_bytes)
        .map(|(value, _)| value)
}

#[cfg(test)]
pub(crate) fn mcp_show_event_with_compact(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> anyhow::Result<(Value, Value)> {
    mcp_show_event_application(data_root, id, before, after, window, output_limit_bytes)
        .map_err(ShowApplicationError::into_cli_error)
}

pub(crate) fn mcp_show_event_application(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> ShowApplicationResult<(Value, Value)> {
    let index = open_index(data_root).map_err(ShowApplicationError::from_application_error)?;
    let compact = CompactPresentation::open(&index, &index_root(data_root))
        .map_err(ShowApplicationError::from_application_error)?;
    let query = PinnedHistoryQuery::new(&index, compact.retained_peer());
    let result = query
        .show_event(&ShowEventRequest {
            selector: id.to_owned(),
            before,
            after,
            window,
            budget: EventWindowBudget {
                maximum_events: ctx_history_index::MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
                maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
                maximum_content_bytes: output_limit_bytes,
            },
        })
        .map_err(externalize_query_error)
        .map_err(ShowApplicationError::from_application_error)?;
    let selected = result.selected;
    let events = result.events;
    let value = event_window_json_with_lineage(
        &selected,
        &events,
        &result.copied_lineage,
        OutputFormat::Json,
        output_limit_bytes,
    )
    .map_err(ShowApplicationError::from_application_error)?;
    enforce_json_output_limit(&value, output_limit_bytes, selected.event_id.as_uuid())
        .map_err(ShowApplicationError::from_application_error)?;
    let compact_value = compact
        .project(&value)
        .map_err(ShowApplicationError::from_application_error)?;
    Ok((value, compact_value))
}
