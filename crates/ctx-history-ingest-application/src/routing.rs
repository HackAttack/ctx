use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::{
    validate_provider_source_roots_outside_data_root, DiscoveryReport, ProviderSource,
    ProviderSourceStatus,
};

use crate::{HistorySourcePluginSource, SourceStats};

/// Coarse discovery boundary. Implementations return one fully assembled
/// snapshot and must not expose per-record callbacks.
pub trait SourceDiscoveryPort {
    fn discover_all(&self) -> Result<DiscoveryReport>;
    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport>;
}

/// Provider-owned exact-path and plugin admission boundary. Admission is
/// request-scoped: it never registers a durable automatic root.
pub trait CaptureAdmissionPort {
    type Admission;
    fn explicit_source(
        &self,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource>;
    fn prepare_plugin(
        &self,
        source: HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource>;
    fn admit_exact(
        &self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<Self::Admission>;
}

/// Exactly one logical publication request boundary. Implementations own
/// daemon coordination and receipt pin verification; application code supplies
/// no parser, process, or serialization callback.
pub trait IngestRefreshPort {
    type Admission;
    type Publication;
    fn refresh(
        &mut self,
        data_root: &Path,
        admission: Option<&Self::Admission>,
        no_daemon: bool,
        progress: &mut dyn IngestProgressPort,
    ) -> Result<Self::Publication>;
}

/// Bounded operation-level progress boundary; no source record is permitted to
/// trigger a call through this port.
pub trait IngestProgressPort {
    fn message(&mut self, stage: &'static str, message: String) -> Result<()>;
}

/// Result of the one bounded automatic safety discovery. It is intentionally
/// separate from data-root initialization so callers can reject unsafe roots
/// without creating ctx state.
#[derive(Debug, Clone)]
pub struct AutomaticSourcePreflight {
    pub sources: Vec<ProviderSource>,
    pub has_importable_source: bool,
    pub hermes_only_candidate: Option<ProviderSource>,
}

pub fn automatic_source_preflight(
    discovery: &dyn SourceDiscoveryPort,
    data_root: &Path,
) -> Result<AutomaticSourcePreflight> {
    let snapshot = discovery.discover_all()?;
    validate_provider_source_roots_outside_data_root(data_root, snapshot.sources.iter())?;
    let has_importable_source = snapshot.sources.iter().any(|source| {
        source.exists
            && source.status == ProviderSourceStatus::Available
            && source.import_support.is_importable()
    });
    let hermes_only_candidate = snapshot
        .sources
        .iter()
        .find(|source| {
            source.provider == CaptureProvider::Hermes
                && source.exists
                && source.status == ProviderSourceStatus::Unsupported
        })
        .cloned();
    Ok(AutomaticSourcePreflight {
        sources: snapshot.sources,
        has_importable_source,
        hermes_only_candidate,
    })
}

#[derive(Debug, Clone, Default)]
pub struct IngestRequest {
    pub path: Option<PathBuf>,
    pub provider: Option<CaptureProvider>,
    pub custom_jsonl: bool,
    pub history_source: Option<String>,
    pub history_source_manifests: Vec<PathBuf>,
    pub all: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRoute {
    Automatic,
    ExplicitPath,
    HistorySourcePlugin,
}

pub fn validate_ingest_request(request: &IngestRequest) -> Result<IngestRoute> {
    if request.custom_jsonl && request.path.is_none() {
        return Err(anyhow!(
            "ctx import --input-format requires --path for a source-backed catalog entry"
        ));
    }
    if request.path.is_some() && !request.custom_jsonl && request.provider.is_none() {
        return Err(anyhow!("ctx import --path requires --provider for native provider history; use `ctx import --provider codex --path <path>` or `ctx import --input-format ctx-history-jsonl-v1 --path <file>"));
    }
    if request.history_source.is_some() || !request.history_source_manifests.is_empty() {
        Ok(IngestRoute::HistorySourcePlugin)
    } else if request.path.is_some() {
        Ok(IngestRoute::ExplicitPath)
    } else {
        Ok(IngestRoute::Automatic)
    }
}

/// Neutral facts sent to telemetry delivery by the CLI after an outcome has
/// been rendered. These deliberately contain no telemetry client dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestTelemetryFacts {
    pub sources_seen: u64,
    pub source_files: u64,
    pub source_bytes: u64,
    pub failed_sources: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestTotals {
    pub source_files: usize,
    pub source_bytes: u64,
    pub imported_sources: usize,
    pub failed_sources: usize,
    pub rejected_records: u64,
    pub generation_changed: bool,
}
impl IngestTotals {
    pub fn from_source(stats: SourceStats) -> Self {
        Self {
            source_files: stats.files,
            source_bytes: stats.bytes,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_rejects_format_without_path_before_any_port() {
        let request = IngestRequest {
            custom_jsonl: true,
            ..IngestRequest::default()
        };
        assert!(validate_ingest_request(&request).is_err());
    }
    #[test]
    fn plugin_route_wins_over_automatic() {
        let request = IngestRequest {
            history_source: Some("x/y".into()),
            ..IngestRequest::default()
        };
        assert_eq!(
            validate_ingest_request(&request).unwrap(),
            IngestRoute::HistorySourcePlugin
        );
    }
    #[test]
    fn exact_path_remains_one_shot_route() {
        let request = IngestRequest {
            path: Some("history.jsonl".into()),
            provider: Some(CaptureProvider::Codex),
            ..IngestRequest::default()
        };
        assert_eq!(
            validate_ingest_request(&request).unwrap(),
            IngestRoute::ExplicitPath
        );
    }

    #[test]
    fn unsafe_roots_fail_before_any_admission_or_refresh_port_exists() {
        struct Unsafe;
        impl SourceDiscoveryPort for Unsafe {
            fn discover_all(&self) -> Result<DiscoveryReport> {
                Ok(DiscoveryReport::default())
            }
            fn discover_provider(&self, _: CaptureProvider) -> Result<DiscoveryReport> {
                unreachable!()
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let preflight = automatic_source_preflight(&Unsafe, temp.path()).unwrap();
        assert!(preflight.sources.is_empty());
    }
}
