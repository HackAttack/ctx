#![allow(
    dead_code,
    unused_imports,
    reason = "the compatibility surface remains until SQLite provider packs import provider-runtime directly"
)]

pub(crate) use ctx_history_provider_runtime::{
    combine_sqlite_errors, combine_sqlite_finalization, ensure_sqlite_table_columns,
    map_sqlite_source_access_error, open_provider_sqlite_readonly, optional_column_expr,
    optional_text_column_expr, optional_timestamp_millis_expr, sqlite_component_change_token,
    sqlite_ident, sqlite_retry_decision, sqlite_schema_fingerprint, sqlite_table_columns,
    sqlite_table_exists, ReadOnlySqliteConnection, SqliteLengthPreflightGuard,
    SqliteLogicalSnapshot, SqliteRetryDecision, SqliteSourceAccessError,
};
