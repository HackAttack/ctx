#![allow(
    dead_code,
    unused_imports,
    reason = "the compatibility surface remains until provider packs depend on source I/O directly"
)]

use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{BufRead, Take},
    path::Path,
    time::SystemTime,
};

#[cfg(test)]
use std::path::PathBuf;

use serde_json::Value;

use crate::{CaptureError, Result};

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

#[derive(Debug, Clone)]
pub(crate) struct ProviderSourceRoot(ctx_history_source_io::ProviderSourceRoot);

#[derive(Debug)]
pub(crate) struct ProviderSourceDirectory(ctx_history_source_io::ProviderSourceDirectory);

#[derive(Debug)]
pub(crate) struct OpenedProviderSourceFile(ctx_history_source_io::OpenedProviderSourceFile);

#[derive(Debug)]
pub(crate) enum OpenedProviderSourcePath {
    File(OpenedProviderSourceFile),
    Directory(ProviderSourceDirectory),
}

impl From<ctx_history_source_io::OpenedProviderSourcePath> for OpenedProviderSourcePath {
    fn from(opened: ctx_history_source_io::OpenedProviderSourcePath) -> Self {
        match opened {
            ctx_history_source_io::OpenedProviderSourcePath::File(file) => {
                Self::File(OpenedProviderSourceFile(file))
            }
            ctx_history_source_io::OpenedProviderSourcePath::Directory(directory) => {
                Self::Directory(ProviderSourceDirectory(directory))
            }
        }
    }
}

impl OpenedProviderSourcePath {
    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        match self {
            Self::File(file) => file.authority_fingerprint(),
            Self::Directory(directory) => directory.authority_fingerprint(),
        }
    }
}

impl ProviderSourceRoot {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        ctx_history_source_io::ProviderSourceRoot::open(path)
            .map(Self)
            .map_err(Into::into)
    }

    pub(crate) fn named_path(&self) -> &Path {
        self.0.named_path()
    }

    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub(crate) fn same_object_as(&self, other: &Self) -> bool {
        self.0.same_object_as(&other.0)
    }

    pub(crate) fn directory(&self) -> Result<ProviderSourceDirectory> {
        self.0
            .directory()
            .map(ProviderSourceDirectory)
            .map_err(Into::into)
    }

    pub(crate) fn open_path(&self, relative_path: &Path) -> Result<OpenedProviderSourcePath> {
        self.0
            .open_path(relative_path)
            .map(Into::into)
            .map_err(Into::into)
    }

    pub(crate) fn open_file(&self, relative_path: &Path) -> Result<OpenedProviderSourceFile> {
        self.0
            .open_file(relative_path)
            .map(OpenedProviderSourceFile)
            .map_err(Into::into)
    }

    pub(crate) fn open_directory(&self, relative_path: &Path) -> Result<ProviderSourceDirectory> {
        self.0
            .open_directory(relative_path)
            .map(ProviderSourceDirectory)
            .map_err(Into::into)
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        self.0.revalidate().map_err(Into::into)
    }

    pub(crate) fn revalidate_same_object(&self) -> Result<()> {
        self.0.revalidate_same_object().map_err(Into::into)
    }
}

impl ProviderSourceDirectory {
    pub(crate) fn authority_root(&self) -> ProviderSourceRoot {
        ProviderSourceRoot(self.0.authority_root())
    }

    pub(crate) fn relative_path(&self) -> &Path {
        self.0.relative_path()
    }

    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub(crate) fn try_clone_authority_handle(&self) -> std::io::Result<File> {
        self.0.try_clone_authority_handle()
    }

    pub(crate) fn entries(&self, maximum_entries: usize) -> Result<Vec<OsString>> {
        self.0.entries(maximum_entries).map_err(Into::into)
    }

    pub(crate) fn open_child(&self, name: &OsStr) -> Result<OpenedProviderSourcePath> {
        self.0.open_child(name).map(Into::into).map_err(Into::into)
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        self.0.revalidate().map_err(Into::into)
    }
}

impl OpenedProviderSourceFile {
    pub(crate) fn len(&self) -> u64 {
        self.0.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn modified(&self) -> std::io::Result<SystemTime> {
        self.0.modified()
    }

    pub(crate) fn metadata(&self) -> &Metadata {
        self.0.metadata()
    }

    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub(crate) fn ordinary_file_token(&self) -> [u8; 32] {
        self.0.ordinary_file_token()
    }

    pub(crate) fn current_ordinary_file_token(&self) -> Result<[u8; 32]> {
        self.0.current_ordinary_file_token().map_err(Into::into)
    }

    pub(crate) fn file(&self) -> &File {
        self.0.file()
    }

    pub(crate) fn reopen_same_object(&self) -> Result<File> {
        self.0.reopen_same_object().map_err(Into::into)
    }

    pub(crate) fn bounded_reader(&self, maximum_bytes: u64) -> Result<Take<File>> {
        self.0.bounded_reader(maximum_bytes).map_err(Into::into)
    }

    pub(crate) fn read_all_bounded(&self, maximum_bytes: usize) -> Result<Vec<u8>> {
        self.0.read_all_bounded(maximum_bytes).map_err(Into::into)
    }

    pub(crate) fn read_exact_range(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        self.0
            .read_exact_range(offset, length, maximum_bytes)
            .map_err(Into::into)
    }

    pub(crate) fn read_exact_range_allow_append(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        self.0
            .read_exact_range_allow_append(offset, length, maximum_bytes)
            .map_err(Into::into)
    }

    pub(crate) fn revalidate_leaf(&self) -> Result<()> {
        self.0.revalidate_leaf().map_err(Into::into)
    }

    pub(crate) fn revalidate_same_object_leaf(&self) -> Result<()> {
        self.0.revalidate_same_object_leaf().map_err(Into::into)
    }

    pub(crate) fn revalidate_same_object(&self) -> Result<()> {
        self.0.revalidate_same_object().map_err(Into::into)
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        self.0.revalidate().map_err(Into::into)
    }
}

pub(crate) fn open_provider_source_file(path: &Path) -> Result<OpenedProviderSourceFile> {
    ctx_history_source_io::open_provider_source_file(path)
        .map(OpenedProviderSourceFile)
        .map_err(Into::into)
}

pub(crate) fn open_provider_source_path(path: &Path) -> Result<OpenedProviderSourcePath> {
    ctx_history_source_io::open_provider_source_path(path)
        .map(Into::into)
        .map_err(Into::into)
}

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

pub(crate) fn inventory_provider_jsonl_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_jsonl_paths(root, limits).map_err(Into::into)
}

pub(crate) fn inventory_provider_regular_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_regular_paths(root, limits).map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn collect_jsonl_paths_bounded(
    root: &Path,
    paths: &mut Vec<PathBuf>,
    max_paths: usize,
) -> Result<()> {
    ctx_history_source_io::collect_jsonl_paths_bounded(root, paths, max_paths).map_err(Into::into)
}

pub(crate) fn provider_regular_file_len(path: &Path) -> Result<u64> {
    ctx_history_source_io::provider_regular_file_len(path).map_err(Into::into)
}

pub(crate) fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    ctx_history_source_io::ensure_regular_provider_transcript_file(path).map_err(Into::into)
}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    ctx_history_source_io::ensure_provider_path_parents_are_not_symlinks(path).map_err(Into::into)
}

pub(crate) fn path_has_component(path: &Path, expected: &str) -> bool {
    ctx_history_source_io::path_has_component(path, expected)
}

pub(crate) fn provider_metadata_is_link_like(metadata: &Metadata) -> bool {
    ctx_history_source_io::provider_metadata_is_link_like(metadata)
}

pub(crate) fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    ctx_history_source_io::read_text_file_limited(path, max_bytes, label).map_err(Into::into)
}

pub(crate) fn read_provider_jsonl_line_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead> {
    ctx_history_source_io::read_provider_jsonl_line_or_skip_oversized(reader, buffer)
        .map_err(Into::into)
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<usize> {
    ctx_history_source_io::discard_provider_jsonl_line(reader).map_err(Into::into)
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    ctx_history_source_io::read_json_file_limited(path, max_bytes, label).map_err(Into::into)
}
