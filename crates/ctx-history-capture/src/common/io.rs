#![allow(
    dead_code,
    unused_imports,
    reason = "the compatibility surface remains until provider packs depend on source I/O directly"
)]

use crate::CaptureError;

pub use ctx_history_source_io::{
    ProviderJsonlInventory, ProviderJsonlInventoryLimits, ProviderJsonlLineRead,
    PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
pub(crate) use ctx_history_source_io::{
    NON_REGULAR_PROVIDER_SOURCE_REASON, REPARSE_PROVIDER_SOURCE_REASON,
    SYMLINK_PROVIDER_SOURCE_REASON,
};

ctx_history_source_io::define_mapped_source_io_compat!(CaptureError);

pub(crate) fn is_non_regular_source_rejection(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
    )
}

pub(crate) fn is_symlink_source_rejection(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == SYMLINK_PROVIDER_SOURCE_REASON || *reason == REPARSE_PROVIDER_SOURCE_REASON
    )
}
