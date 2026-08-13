//! Trae provider parsing and source-backed Core projection.
//!
//! Capture owns discovery, route identity, lifecycle, deferred spooling, and
//! publication. This pack owns only Trae's bounded SQLite observation,
//! parsing, identity construction, and replacement-document adapter.

mod trae;

pub use ctx_history_provider_runtime::{CaptureError, ProviderRuntimeBinding, Result};
pub use trae::json_stream::{trae_payload_admission, TraePayloadAdmission};
pub use trae::nativepath::TraeReplacementTree;

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
pub const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
pub const TRAE_CN_INPUT_HISTORY_KEY: &str = "icube-ai-agent-storage-input-history";
pub const TRAE_CHAT_KEYS: &[&str] = &[
    "memento/icube-ai-agent-storage",
    TRAE_CN_INPUT_HISTORY_KEY,
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];

pub const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;
pub const TRAE_CHAT_ROWS_QUERY: &str =
    "select [key], count(*), typeof(value), coalesce(octet_length(value), 0), \
            case when count(*) = 1 \
                       and typeof(value) = 'text' \
                       and octet_length(value) + octet_length([key]) + ?7 <= ?8 \
                 then cast(value as text) end \
     from ItemTable \
     where [key] in (?1, ?2, ?3, ?4, ?5, ?6) \
     group by [key]";

pub fn trae_sqlite_value_fits_parser_bound(chat_key: &str, retained_bytes: u64) -> bool {
    retained_bytes
        .saturating_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
        .saturating_add(u64::try_from(chat_key.len()).unwrap_or(u64::MAX))
        <= u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX)
}

mod provider_limits {
    pub const TRAE_SOURCE_BACKED_PAGE_MAX_UNITS: usize = 64;
    pub const TRAE_SOURCE_BACKED_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
}

mod sqlite_source {
    pub use ctx_history_source_sqlite::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    };
}

#[cfg(test)]
fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| tempfile::tempdir().expect("provider SQLite test root"))
        .path()
}

#[cfg(test)]
mod test_support_paths {
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::tempdir()
    }
}
