use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderJsonlInventoryLimit {
    Directories,
    Depth,
    EligiblePaths,
    MetadataEntries,
}

impl std::fmt::Display for ProviderJsonlInventoryLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Directories => "directories",
            Self::Depth => "depth",
            Self::EligiblePaths => "eligible_jsonl_paths",
            Self::MetadataEntries => "metadata_entries",
        })
    }
}

#[derive(Debug, Error)]
pub enum SourceIoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ProviderJsonlInventoryLimit,
        maximum: usize,
        observed: usize,
    },
    #[error("system I/O error during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system invariant failed: {0}")]
    SystemInvariant(&'static str),
    #[error("provider source changed during bounded capture")]
    SourceChangedDuringCapture,
    #[error("{primary}; terminal SQLite revalidation/cleanup also failed: {finalization}")]
    SqliteFinalization {
        primary: Box<SourceIoError>,
        finalization: Box<SourceIoError>,
    },
}

pub type Result<T> = std::result::Result<T, SourceIoError>;
