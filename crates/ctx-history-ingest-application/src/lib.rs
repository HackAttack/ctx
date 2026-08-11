//! Transport-neutral application policy for history-source discovery and ingest.
//!
//! This crate owns request routing, bounded source inventory, manifest-backed
//! source discovery, and source-list assembly. Provider parsers, durable
//! source admission, refresh execution, command rendering, and telemetry
//! delivery remain behind coarse borrowed ports in their owning layers.

mod inventory;
mod listing;
mod plugins;
mod routing;
mod totals;

pub use inventory::{source_stats, SourceStats};
pub use listing::{
    assemble_source_listing, history_source_plugin_report, merge_sources, source_identity,
    source_is_visible, HistorySourcePluginReport, HistorySourcePluginReportingStatus,
    SourceListing, SourceListingRequest,
};
pub use plugins::{
    discover_history_source_plugins, discover_history_source_plugins_with_diagnostics,
    select_history_source_plugin, HistorySourcePluginDiscovery, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource, COMMAND_ONLY_UNSUPPORTED_REASON,
};
pub use routing::{
    automatic_source_preflight, validate_ingest_request, AutomaticSourcePreflight,
    CaptureAdmissionPort, IngestProgressPort, IngestRefreshPort, IngestRequest, IngestRoute,
    IngestTelemetryFacts, IngestTotals, SourceDiscoveryPort,
};
pub use totals::ImportTotals;
