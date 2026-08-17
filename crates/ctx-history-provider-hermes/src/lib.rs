//! Hermes history provider for ctx agent history.
//!
//! Hermes owns a shared SQLite database with independently certified session
//! leaves, incremental append, lazy base lookup, publication receipts, and
//! bounded exact reconciliation. The specialized document-tree adapter stays
//! provider-owned while source acquisition and capture lifecycle remain in
//! lower provider-neutral crates.

#![allow(clippy::items_after_test_module)]
#![cfg_attr(any(test, feature = "test-support"), allow(dead_code, unused_imports))]

mod provider;
pub mod registration;

pub use ctx_history_capture_model::{ProviderSource, ProviderSourceStatus};
pub use ctx_history_provider_runtime::{CaptureError, ProviderRouteControlExpectation, Result};
pub use provider::source_backed::{
    hermes_route_control_database_identity, hermes_route_control_exact_due,
    hermes_route_control_exact_due_for_profile,
};

pub fn hermes_automatic_profile_name(
    path: &std::path::Path,
) -> std::result::Result<String, String> {
    provider::source_backed::hermes_automatic_profile_name(path).map_err(|error| error.to_string())
}

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

pub(crate) mod normalization {
    use chrono::{DateTime, Utc};

    use crate::{CaptureError, Result};

    pub(crate) fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
        ctx_history_capture_model::normalization::provider_nonnegative_i64_to_u64(value, field)
            .map_err(CaptureError::InvalidPayload)
    }

    pub(crate) fn provider_required_timestamp_seconds(
        value: f64,
        field: &'static str,
    ) -> Result<DateTime<Utc>> {
        ctx_history_capture_model::normalization::provider_required_timestamp_seconds(value, field)
            .map_err(CaptureError::InvalidPayload)
    }
}

pub(crate) mod source_sqlite {
    use std::collections::BTreeSet;

    #[cfg(test)]
    use std::path::Path;

    use rusqlite::Connection;

    #[cfg(test)]
    use crate::CaptureError;
    use crate::Result;

    pub(crate) use ctx_history_source_sqlite::SqliteLengthPreflightGuard;

    pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
        ctx_history_source_sqlite::sqlite_table_exists(conn, table).map_err(Into::into)
    }

    pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
        ctx_history_source_sqlite::sqlite_table_columns(conn, table).map_err(Into::into)
    }

    pub(crate) fn ensure_sqlite_table_columns(
        columns: &BTreeSet<String>,
        label: &str,
        required: &[&str],
    ) -> Result<()> {
        ctx_history_source_sqlite::ensure_sqlite_table_columns(columns, label, required)
            .map_err(Into::into)
    }

    pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
        ctx_history_source_sqlite::sqlite_schema_fingerprint(conn).map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) type ReadOnlySqliteConnection =
        ctx_history_source_sqlite::MappedReadOnlySqliteConnection<CaptureError>;

    #[cfg(test)]
    pub(crate) fn open_provider_sqlite_readonly(
        data_root: &Path,
        path: &Path,
    ) -> Result<ReadOnlySqliteConnection> {
        ReadOnlySqliteConnection::open(data_root, path)
    }
}

pub(crate) mod source_backed {
    pub(crate) use crate::lifecycle::*;
    pub(crate) use ctx_history_capture_runtime::combine_primary_and_cleanup_route_errors;

    pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
        SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
    }

    pub(crate) fn sqlite_source_progress(
        progress: ctx_history_source_sqlite::SqliteSourceProgress,
    ) -> SourceBackedCurrentSourceProgress {
        let stage = match progress.stage {
            ctx_history_source_sqlite::SqliteSourceProgressStage::SourceFamilyCopy => {
                SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            }
        };
        SourceBackedCurrentSourceProgress {
            stage,
            snapshot_pages_completed: progress.snapshot_pages_completed,
            snapshot_pages_total: progress.snapshot_pages_total,
            snapshot_bytes_completed: progress.snapshot_bytes_completed,
            snapshot_bytes_total: progress.snapshot_bytes_total,
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        }
    }

    pub(crate) mod family {
        pub(crate) mod document {
            pub(crate) use crate::lifecycle::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentAppendBase, DocumentBaseRoute,
                DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal,
                ObservedDocumentLeaf, ReplacementDocumentTree,
            };
        }
    }
}

pub(crate) mod native_ingestion {
    pub(crate) const NATIVE_INGESTION_PAGE_MAX_UNITS: usize =
        crate::NATIVE_INGESTION_PAGE_MAX_UNITS;
    pub(crate) const NATIVE_INGESTION_PAGE_MAX_BYTES: usize =
        crate::NATIVE_INGESTION_PAGE_MAX_BYTES;
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
        crate::provider::source_backed::reset_logical_row_traversals();
        crate::provider::source_backed::reset_base_route_source_visits();
        crate::provider::reset_exact_message_query_counters();
    }

    pub fn hermes_work_counters() -> HermesWorkCounters {
        HermesWorkCounters {
            logical_row_traversals: crate::provider::source_backed::logical_row_traversals(),
            inventory_observation_rows: crate::provider::source_backed::inventory_observation_rows(
            ),
            document_base_route_source_visits:
                crate::provider::source_backed::base_route_source_visits(),
            session_scans: crate::provider::source_backed::session_scan_receipts(),
            exact_message_queries: crate::provider::exact_message_query_counters(),
            exact_message_spools: crate::provider::exact_message_spool_counters(),
        }
    }

    pub fn set_before_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
        crate::provider::source_backed::replacement::set_before_hermes_snapshot_seal_hook(hook);
    }

    pub fn set_after_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
        crate::provider::source_backed::replacement::set_after_hermes_snapshot_seal_hook(hook);
    }
}
