//! Compatibility adapters for final-binary callers that have not moved yet.

use std::path::{Path, PathBuf};

use anyhow::Result;
use ctx_history_capture::DiscoveryReport;
use ctx_history_core::CaptureProvider;

pub(crate) use ctx_history_cli::{
    discovery_report_issues_json, history_source_plugin_report, manual_path_guidance,
    plugin_manifest_failures_json, plugin_sources_json, provider_cli_name, sources_json,
    SourceInfo,
};

pub(crate) struct CliSourceDiscoveryPort(ctx_history_cli::CliSourceDiscoveryPort);

impl CliSourceDiscoveryPort {
    pub(crate) fn new(home: Option<PathBuf>) -> Self {
        Self(ctx_history_cli::CliSourceDiscoveryPort::new(home))
    }
}

impl ctx_history_ingest_application::SourceDiscoveryPort for CliSourceDiscoveryPort {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        ctx_history_ingest_application::SourceDiscoveryPort::discover_all(&self.0)
    }

    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport> {
        ctx_history_ingest_application::SourceDiscoveryPort::discover_provider(&self.0, provider)
    }

    fn provider_selection_guidance(
        &self,
        provider: CaptureProvider,
    ) -> ctx_history_ingest_application::ProviderSelectionGuidance {
        ctx_history_ingest_application::SourceDiscoveryPort::provider_selection_guidance(
            &self.0, provider,
        )
    }
}

pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<serde_json::Value>> {
    ctx_history_cli::discovered_plugin_sources_json(data_root)
}

pub(crate) fn discovered_sources_report() -> DiscoveryReport {
    ctx_history_cli::discovered_sources_report(crate::identity::home_dir().as_deref())
}
