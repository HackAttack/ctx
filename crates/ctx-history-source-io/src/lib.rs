//! Provider-neutral, policy-free source filesystem access for history capture.
//!
//! This crate owns bounded ordinary-file and tree reads, retained source
//! authority handles, event-file inventories, and read-only SQLite snapshots.
//! Provider discovery and parsing policy remain in their owning crates.

#![cfg_attr(feature = "test-support", allow(dead_code, unused_imports))]

mod error;
mod event_files;
mod io;
mod mapped_io;
mod ordinary_file;
mod progress;
mod sqlite;
mod sqlite_source;

pub use error::ProviderJsonlInventoryLimit as SourceIoJsonlInventoryLimit;
pub use error::{ProviderJsonlInventoryLimit, Result, SourceIoError};
pub use event_files::*;
pub use io::*;
pub use mapped_io::*;
pub use ordinary_file::*;
pub use progress::{SqliteSourceProgress, SqliteSourceProgressStage};
pub use sqlite::*;
pub use sqlite_source::*;

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;

#[cfg(any(test, feature = "test-support"))]
mod test_support_paths;

#[cfg(any(test, feature = "test-support"))]
fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| test_support_paths::tempdir().expect("provider SQLite test root"))
        .path()
}
