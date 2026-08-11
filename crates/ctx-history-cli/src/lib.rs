//! Transport-neutral command contracts for local agent-history operations.
//!
//! Clap parsing, final analytics delivery, and product-specific host composition
//! remain outside this crate. This boundary owns only plain command values and
//! the ports future command bodies use to request configuration, terminal I/O,
//! and observations.

mod analytics;
mod cli;
mod config;
mod dispatch;
mod history_source_plugins;
mod list_events;
mod local_usage;
mod mcp_tool_call;
mod output;
mod ports;
mod presentation_limit;
mod provider_args;
mod request;
mod search_filters;
mod semantic;
mod source_index;
mod transcript;
mod ui;

pub use cli::{
    ContentScopeArg, LocateArgs, LocateEventArgs, LocateSessionArgs, LocateTarget, SearchArgs,
    SearchBackendArg, ShowArgs, ShowEventArgs, ShowSessionArgs, ShowTarget,
};
pub use list_events::{
    decode_cursor, event_query_error_value, event_range_page_value,
    mcp_event_query_core_record_bytes, render_event, run as run_list_events,
    selection as list_events_selection,
    selection_from_request as list_events_selection_from_request,
    validated_limit as validated_event_limit, EventContentProjection, EventContentProjectionArg,
    EventQueryDirection, EventQueryError, EventQueryFormat, EventQueryScope, EventQueryWireRequest,
    ListEventsArgs, DEFAULT_EVENT_QUERY_LIMIT,
};
pub use provider_args::ProviderArg;
pub use source_index::{
    copied_lineage_summary, generation_query_authority_error_json, mcp_search_with_compact,
    mcp_show_event_application, mcp_show_session_application, normalize_mcp_search_request,
    run_locate, run_search, run_search_with_observations, run_show, source_search_request,
    validate_explicit_semantic_scope, McpSearchError, ShowApplicationError, SourceSearchRequest,
};

pub use config::{ConfigPortError, HistoryCliConfig, HistoryCliConfigPort};
pub use history_source_plugins::{
    discover_history_source_plugins_with_diagnostics, prepare_source_backed_history_source,
    HistorySourcePluginManifestFailure, HistorySourcePluginRefresh, HistorySourcePluginSource,
    PreparedHistorySourcePluginRefresh,
};
pub use mcp_tool_call::{
    append_mcp_tool_call_markdown, append_mcp_tool_call_text, escape_markdown_structure,
    mcp_tool_call_display, McpToolCallDisplay, MCP_TOOL_CALL_DISPLAY_MAX_CHARS,
    MCP_TOOL_CALL_JSON_GUIDANCE, MCP_TOOL_CALL_STRUCTURED_GUIDANCE,
};
pub use output::JsonOutputFormat;
pub use ports::{
    HistoryCliObservation, HistoryCliObservationValue, HistoryCliOperation, HistoryCliRuntimePort,
    ObservabilityPort, OutputStream, RefreshObservationMode, RefreshObservationStatus,
    SearchContextObservation, SearchExecutionObservation, TerminalPort,
};
pub use request::{
    HistoryProvider, ImportFormat, ImportRequest, ListEventsContentProjection, ListEventsDirection,
    ListEventsRequest, ListEventsScope, ListRequest, LocateRequest, OutputFormat, ProgressMode,
    RefreshMode, SearchBackend, SearchContentScope, SearchRequest, SetupRequest, ShowRequest,
    SourceIndexRequest, SourcesRequest, TranscriptMode,
};
pub use search_filters::parse_since_filter;
pub use transcript::{shell_quote_arg, write_output, TranscriptOutput};

/// Marks a failure whose command-specific output has already been written.
/// The final `ctx` dispatch maps this marker to its normal failure exit once,
/// without rendering a second diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub struct RenderedCliError;
