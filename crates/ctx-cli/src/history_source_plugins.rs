//! CLI adapter for provider-owned history-source admission.
//!
//! Manifest discovery and selection live in `ctx-history-ingest-application`.
//! The retained submodule owns only provider-side durable-path validation and
//! JSONL record admission, pending convergence with the capture layer.

mod source_backed;

pub(crate) use ctx_history_ingest_application::{
    discover_history_source_plugins_with_diagnostics, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource,
};
pub(crate) use source_backed::prepare_source_backed_history_source;
