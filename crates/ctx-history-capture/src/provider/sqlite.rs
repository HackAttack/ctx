#![allow(
    dead_code,
    reason = "the compatibility surface remains until SQLite provider packs depend on source I/O directly"
)]

use std::{collections::BTreeSet, path::Path};

use rusqlite::Connection;

pub(crate) use ctx_history_source_sqlite::{
    optional_column_expr, optional_text_column_expr, optional_timestamp_millis_expr, sqlite_ident,
    SqliteLengthPreflightGuard, SqliteLogicalSnapshot, SqliteSourceAccessError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteRetryDecision {
    DoNotRetry,
    DoNotRetryCorrupt,
    RetryBusyOrLocked,
    RetrySourceTransition,
    RouteFatalResource,
}

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

use crate::{CaptureError, Result};

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

pub(crate) fn sqlite_component_change_token(
    path: &Path,
    observation: &ctx_history_source_io::OrdinaryFileObservation,
) -> Result<[u8; 32]> {
    ctx_history_source_sqlite::sqlite_component_change_token(path, observation).map_err(Into::into)
}

pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
    ctx_history_source_sqlite::sqlite_schema_fingerprint(conn).map_err(Into::into)
}

pub(crate) type ReadOnlySqliteConnection =
    ctx_history_source_sqlite::MappedReadOnlySqliteConnection<CaptureError>;

pub(crate) fn combine_sqlite_finalization<T>(
    primary: Result<T>,
    finalization: Result<()>,
) -> Result<T> {
    match (primary, finalization) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(finalization)) => Err(finalization),
        (Err(primary), Err(finalization)) => Err(combine_sqlite_errors(primary, finalization)),
    }
}

pub(crate) fn combine_sqlite_errors(
    primary: CaptureError,
    finalization: CaptureError,
) -> CaptureError {
    CaptureError::SqliteFinalization {
        primary: Box::new(primary),
        finalization: Box::new(finalization),
    }
}

pub(crate) fn open_provider_sqlite_readonly(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    ReadOnlySqliteConnection::open(data_root, path)
}

pub(crate) fn map_sqlite_source_access_error(error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::Io { source, .. } => CaptureError::Io(source),
        SqliteSourceAccessError::Sqlite { source, .. } => CaptureError::Sqlite(source),
        SqliteSourceAccessError::UnsafeFile { path, reason } => {
            CaptureError::InvalidProviderTranscriptPath { path, reason }
        }
        SqliteSourceAccessError::ConnectionIdentityMismatch
        | SqliteSourceAccessError::SourceChanged => CaptureError::SourceChangedDuringCapture,
        SqliteSourceAccessError::SnapshotNotActive => {
            CaptureError::SystemInvariant("provider SQLite source snapshot is inactive")
        }
        other => CaptureError::SystemIo {
            operation: "opening a root-authorized provider SQLite snapshot",
            source: std::io::Error::other(other),
        },
    }
}
