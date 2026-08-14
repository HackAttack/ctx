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

pub(crate) mod sqlite {
    use std::collections::BTreeSet;

    #[cfg(test)]
    use std::path::Path;

    use rusqlite::Connection;

    #[cfg(test)]
    use crate::CaptureError;
    use crate::Result;

    pub(crate) use ctx_history_source_sqlite::{
        optional_text_column_expr, optional_timestamp_millis_expr, SqliteLengthPreflightGuard,
    };

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

    pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
        SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
    }

    pub(crate) fn combine_primary_and_cleanup_route_errors(
        primary: SourceBackedRouteError,
        cleanup: SourceBackedRouteError,
    ) -> SourceBackedRouteError {
        let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
            primary.kind
        } else {
            cleanup.kind
        };
        SourceBackedRouteError::new(
            kind,
            format!(
                "{}; explicit SQLite snapshot cleanup also failed: {}",
                primary.detail, cleanup.detail
            ),
        )
    }

    const fn route_error_severity(kind: SourceBackedRouteErrorKind) -> u8 {
        match kind {
            SourceBackedRouteErrorKind::Internal => 6,
            SourceBackedRouteErrorKind::ResourceUnavailable => 5,
            SourceBackedRouteErrorKind::SourceChanged => 4,
            SourceBackedRouteErrorKind::InvalidSource => 3,
            SourceBackedRouteErrorKind::Unsupported => 2,
            SourceBackedRouteErrorKind::Unavailable => 1,
        }
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
                DocumentLeafExecutionPolicy, DocumentLeafFingerprint, DocumentRecordSpool,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
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

pub mod providers;
