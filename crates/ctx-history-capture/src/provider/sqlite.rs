#![allow(
    dead_code,
    reason = "the compatibility surface remains until SQLite provider packs depend on source I/O directly"
)]

use std::{collections::BTreeSet, ops::Deref, path::Path};

use rusqlite::Connection;

pub(crate) use ctx_history_source_io::{
    optional_column_expr, optional_text_column_expr, optional_timestamp_millis_expr, sqlite_ident,
    SqliteLengthPreflightGuard, SqliteSourceAccessError,
};

use crate::{provider_sources::OrdinaryFileObservation, CaptureError, Result};

pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    ctx_history_source_io::sqlite_table_exists(conn, table).map_err(Into::into)
}

pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    ctx_history_source_io::sqlite_table_columns(conn, table).map_err(Into::into)
}

pub(crate) fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    ctx_history_source_io::ensure_sqlite_table_columns(columns, label, required).map_err(Into::into)
}

pub(crate) fn sqlite_component_change_token(
    path: &Path,
    observation: &OrdinaryFileObservation,
) -> Result<[u8; 32]> {
    ctx_history_source_io::sqlite_component_change_token(path, observation).map_err(Into::into)
}

pub(crate) struct ReadOnlySqliteConnection(ctx_history_source_io::ReadOnlySqliteConnection);

impl Deref for ReadOnlySqliteConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub(crate) fn open_provider_sqlite_readonly(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    ctx_history_source_io::open_provider_sqlite_readonly(data_root, path)
        .map(ReadOnlySqliteConnection)
        .map_err(Into::into)
}

pub(crate) fn open_sqlite_readonly_source(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    ctx_history_source_io::open_sqlite_readonly_source(data_root, path)
        .map(ReadOnlySqliteConnection)
        .map_err(Into::into)
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

pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
    ctx_history_source_io::sqlite_schema_fingerprint(conn).map_err(Into::into)
}
