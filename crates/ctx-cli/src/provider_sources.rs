//! Compatibility adapters for final-binary callers that have not moved yet.

use std::path::Path;

use anyhow::Result;
use ctx_history_capture::DiscoveryReport;

pub(crate) use ctx_history_cli::{discovery_report_issues_json, sources_json};

pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<serde_json::Value>> {
    ctx_history_cli::discovered_plugin_sources_json(data_root)
}

pub(crate) fn discovered_sources_report(data_root: &Path) -> DiscoveryReport {
    ctx_history_cli::discovered_sources_report_with_data_root(
        crate::identity::home_dir().as_deref(),
        data_root,
    )
}
