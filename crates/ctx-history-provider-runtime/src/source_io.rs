use std::{io::BufRead, path::Path};

use crate::{CaptureError, Result};

pub use ctx_history_source_io::{
    ProviderJsonlInventory, ProviderJsonlInventoryLimits, ProviderJsonlLineRead,
    PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

pub type ProviderSourceRoot = ctx_history_source_io::MappedProviderSourceRoot<CaptureError>;
pub type ProviderSourceDirectory =
    ctx_history_source_io::MappedProviderSourceDirectory<CaptureError>;
pub type OpenedProviderSourceFile =
    ctx_history_source_io::MappedOpenedProviderSourceFile<CaptureError>;
pub type OpenedProviderSourcePath =
    ctx_history_source_io::MappedOpenedProviderSourcePath<CaptureError>;

pub fn open_provider_source_file(path: &Path) -> Result<OpenedProviderSourceFile> {
    ctx_history_source_io::open_provider_source_file_mapped(path)
}

pub fn open_provider_source_path(path: &Path) -> Result<OpenedProviderSourcePath> {
    ctx_history_source_io::open_provider_source_path_mapped(path)
}

pub fn inventory_provider_jsonl_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_jsonl_paths_mapped(root, limits)
}

pub fn inventory_provider_regular_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_regular_paths_mapped(root, limits)
}

pub fn provider_regular_file_len(path: &Path) -> Result<u64> {
    ctx_history_source_io::provider_regular_file_len_mapped(path)
}

pub fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    ctx_history_source_io::ensure_regular_provider_transcript_file_mapped(path)
}

pub fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    ctx_history_source_io::ensure_provider_path_parents_are_not_symlinks_mapped(path)
}

pub fn path_has_component(path: &Path, expected: &str) -> bool {
    ctx_history_source_io::path_has_component(path, expected)
}

pub fn provider_metadata_is_link_like(metadata: &std::fs::Metadata) -> bool {
    ctx_history_source_io::provider_metadata_is_link_like(metadata)
}

pub fn read_text_file_limited(path: &Path, maximum: usize, label: &str) -> Result<String> {
    ctx_history_source_io::read_text_file_limited_mapped(path, maximum, label)
}

pub fn read_provider_jsonl_line_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead> {
    ctx_history_source_io::read_provider_jsonl_line_or_skip_oversized_mapped(reader, buffer)
}

pub fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<usize> {
    ctx_history_source_io::discard_provider_jsonl_line_mapped(reader)
}

pub fn read_json_file_limited(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<serde_json::Value> {
    ctx_history_source_io::read_json_file_limited_mapped(path, maximum, label)
}

pub fn is_non_regular_source_rejection(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == ctx_history_source_io::NON_REGULAR_PROVIDER_SOURCE_REASON
    )
}

pub fn is_symlink_source_rejection(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == ctx_history_source_io::SYMLINK_PROVIDER_SOURCE_REASON
                || *reason == ctx_history_source_io::REPARSE_PROVIDER_SOURCE_REASON
    )
}
