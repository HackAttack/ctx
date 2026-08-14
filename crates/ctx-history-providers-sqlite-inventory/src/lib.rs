//! Finite-inventory SQLite providers for ctx agent history.
//!
//! The crate owns AstrBot, Crush, Lingma, Shelley, Hermes, and their bounded
//! provider registration fragments. It depends only on provider-neutral
//! capture and source layers; the capture facade supplies the concrete index
//! lifecycle when composing routes.

#![allow(clippy::items_after_test_module)]
#![cfg_attr(any(test, feature = "test-support"), allow(dead_code, unused_imports))]

mod native_source;
pub mod provider;
pub mod registration;

pub use ctx_history_capture_model::{
    fnv1a64, DiscoveryReport, OutputOutcome, OutputOutcomeMetadata, ProviderSource,
    ProviderSourceStatus,
};
pub use ctx_history_provider_runtime::{CaptureError, Result};
pub use ctx_history_source_discovery::DiscoveryContext;

pub(crate) fn discover_provider_sources_for_provider_with_context(
    context: &DiscoveryContext,
    provider: ctx_history_core::CaptureProvider,
) -> DiscoveryReport {
    // The pack only asks for its own provider cohort. The frozen lower catalog
    // contains those probes directly; unrelated Cursor/Trae fragments are not
    // linked into the pack.
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &SQLITE_INVENTORY_DISCOVERY_PROBES,
        context,
        provider,
    )
}

fn unused_cursor_probe(
    _path: &std::path::Path,
) -> ctx_history_source_discovery::CursorTranscriptProbeOutcome {
    ctx_history_source_discovery::CursorTranscriptProbeOutcome::NotFound
}

fn unused_trae_probe(
    _value: &[u8],
    _key: &str,
) -> ctx_history_source_discovery::TraePayloadProbeOutcome {
    ctx_history_source_discovery::TraePayloadProbeOutcome::Incompatible
}

const SQLITE_INVENTORY_DISCOVERY_PROBES: ctx_history_source_discovery::StaticProviderProbeCatalog =
    ctx_history_source_discovery::StaticProviderProbeCatalog::new(
        ctx_history_source_discovery::CursorProbeFragment::new(unused_cursor_probe),
        ctx_history_source_discovery::TraeProbeFragment::new(
            [""; 6],
            "select 1 where false",
            0,
            unused_trae_probe,
        ),
    );
pub use provider::providers::crush::native_path::source_backed::{
    CrushProjectDatabaseV0, CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0,
    CrushSourceBackedErrorV0, CrushSourceBackedResultV0,
};
pub use provider::providers::hermes::source_backed::{
    hermes_route_control_database_identity, hermes_route_control_exact_due,
    hermes_route_control_exact_due_for_profile,
};

pub fn hermes_automatic_profile_name(
    path: &std::path::Path,
) -> std::result::Result<String, String> {
    provider::providers::hermes::source_backed::hermes_automatic_profile_name(path)
        .map_err(|error| error.to_string())
}

pub const ASTRBOT_SQLITE_SOURCE_FORMAT: &str = "astrbot_data_v4_sqlite";
pub const CRUSH_SQLITE_SOURCE_FORMAT: &str = "crush_sqlite";
pub const LINGMA_SQLITE_SOURCE_FORMAT: &str = "lingma_sqlite";
pub const SHELLEY_SQLITE_SOURCE_FORMAT: &str = "shelley_sqlite";
pub const HERMES_SQLITE_SOURCE_FORMAT: &str = "hermes_state_sqlite";
pub const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize =
    ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;

pub(crate) mod record_evidence {
    pub(crate) use ctx_history_capture_model::RecordDigest;
}

const NATIVE_INGESTION_PAGE_MAX_UNITS: usize = 64;
const NATIVE_INGESTION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

pub mod lifecycle {
    pub use ctx_history_capture_runtime::{
        CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree, DocumentAppendBase,
        DocumentBaseRoute, DocumentLeafExecutionPolicy, DocumentLeafFingerprint,
        DocumentRecordSpool, DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        SourceBackedCoordinatorError, SourceBackedCoordinatorResult,
        SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
        SourceBackedReconciliationDemand, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult, SourceBackedRouteSelection, SourceBackedRouteWatchTargets,
        SourceBackedSelectorAuthority,
    };
}

pub(crate) mod common {
    pub(crate) mod io {
        ctx_history_source_io::define_mapped_source_io_compat!(crate::CaptureError);
    }
}

pub(crate) mod provider_sources {
    pub(crate) use ctx_history_capture_model::ProviderSource;
    pub(crate) use ctx_history_source_sqlite::*;

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SqliteRetryDecision {
        DoNotRetry,
        DoNotRetryCorrupt,
        RetryBusyOrLocked,
        RetrySourceTransition,
        RouteFatalResource,
    }

    #[cfg(test)]
    pub(crate) fn sqlite_retry_decision(error: &SqliteSourceAccessError) -> SqliteRetryDecision {
        if error.is_systemic_resource_failure() {
            SqliteRetryDecision::RouteFatalResource
        } else if error.is_source_changed() {
            SqliteRetryDecision::RetrySourceTransition
        } else if error.is_provider_corruption() || error.is_ctx_owned_corruption() {
            SqliteRetryDecision::DoNotRetryCorrupt
        } else if error.is_busy_or_locked() {
            SqliteRetryDecision::RetryBusyOrLocked
        } else {
            SqliteRetryDecision::DoNotRetry
        }
    }
}

#[cfg(test)]
mod test_support_paths {
    pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::tempdir()
    }
}

#[cfg(test)]
pub(crate) fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| tempfile::tempdir().expect("provider SQLite test root"))
        .path()
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct HermesWorkCounters {
        pub logical_row_traversals: u64,
        pub inventory_observation_rows: u64,
        pub document_base_route_source_visits: u64,
        pub session_scans: BTreeMap<String, (u64, u64)>,
        pub exact_message_queries: (u64, u64),
        pub exact_message_spools: (u64, u64, u64, u64, u64),
    }

    pub fn reset_hermes_work_counters() {
        crate::provider::providers::hermes::source_backed::reset_logical_row_traversals();
        crate::provider::providers::hermes::source_backed::reset_base_route_source_visits();
        crate::provider::providers::hermes::reset_exact_message_query_counters();
    }

    pub fn hermes_work_counters() -> HermesWorkCounters {
        HermesWorkCounters {
            logical_row_traversals:
                crate::provider::providers::hermes::source_backed::logical_row_traversals(),
            inventory_observation_rows:
                crate::provider::providers::hermes::source_backed::inventory_observation_rows(),
            document_base_route_source_visits:
                crate::provider::providers::hermes::source_backed::base_route_source_visits(),
            session_scans: crate::provider::providers::hermes::source_backed::session_scan_receipts(
            ),
            exact_message_queries: crate::provider::providers::hermes::exact_message_query_counters(
            ),
            exact_message_spools: crate::provider::providers::hermes::exact_message_spool_counters(
            ),
        }
    }

    pub fn set_before_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
        crate::provider::providers::hermes::source_backed::replacement::set_before_hermes_snapshot_seal_hook(hook);
    }

    pub fn set_after_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
        crate::provider::providers::hermes::source_backed::replacement::set_after_hermes_snapshot_seal_hook(hook);
    }
}
