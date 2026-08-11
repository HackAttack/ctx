//! CLI-independent provider-owned history-source admission.

mod source_backed;

pub use ctx_history_ingest_application::{
    discover_history_source_plugins_with_diagnostics, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource,
};
pub use source_backed::{prepare_source_backed_history_source, PreparedHistorySourcePluginRefresh};
