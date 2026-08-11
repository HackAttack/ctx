//! Transport-neutral command contracts for local agent-history operations.
//!
//! Clap parsing, final analytics delivery, and product-specific host composition
//! remain outside this crate. This boundary owns only plain command values and
//! the ports future command bodies use to request configuration, terminal I/O,
//! and observations.

mod config;
mod history_source_plugins;
mod ports;
mod request;
mod search_filters;
mod transcript;

pub use config::{ConfigPortError, HistoryCliConfig, HistoryCliConfigPort};
pub use history_source_plugins::{
    discover_history_source_plugins_with_diagnostics, prepare_source_backed_history_source,
    HistorySourcePluginManifestFailure, HistorySourcePluginRefresh, HistorySourcePluginSource,
    PreparedHistorySourcePluginRefresh,
};
pub use ports::{
    HistoryCliObservation, HistoryCliOperation, ObservabilityPort, OutputStream, TerminalPort,
};
pub use request::{
    HistoryProvider, ImportFormat, ImportRequest, ListEventsRequest, ListRequest, LocateRequest,
    OutputFormat, ProgressMode, RefreshMode, SearchRequest, SetupRequest, ShowRequest,
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
