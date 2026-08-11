use std::path::PathBuf;

use ctx_history_capture_model::ProviderSourceFailureKind;
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
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("unsupported capture envelope schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("unsupported provider schema: {0}")]
    UnsupportedSchema(String),
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
    #[error("{0} worker thread panicked")]
    WorkerPanicked(&'static str),
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
    #[error("{primary}; additional SQLite finalization failure: {finalization}")]
    SqliteFinalization {
        primary: Box<CaptureError>,
        finalization: Box<CaptureError>,
    },
    #[error("{provider} source {path:?} failed ({kind}): {detail}")]
    ProviderSource {
        provider: &'static str,
        path: PathBuf,
        kind: ProviderSourceFailureKind,
        detail: String,
    },
    #[error("provider cursor changed during bounded capture")]
    ProviderCursorConflict,
    #[error("line {line} in {path:?} is not a valid capture envelope: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CaptureError>;

impl From<ctx_history_source_io::SourceIoError> for CaptureError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        use ctx_history_source_io::{SourceIoError, SourceIoJsonlInventoryLimit as SourceLimit};

        match error {
            SourceIoError::Io(error) => Self::Io(error),
            SourceIoError::Json(error) => Self::Json(error),
            SourceIoError::Sqlite(error) => Self::Sqlite(error),
            SourceIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SourceIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => Self::ProviderJsonlInventoryLimitExceeded {
                limit: match limit {
                    SourceLimit::Directories => ProviderJsonlInventoryLimit::Directories,
                    SourceLimit::Depth => ProviderJsonlInventoryLimit::Depth,
                    SourceLimit::EligiblePaths => ProviderJsonlInventoryLimit::EligiblePaths,
                    SourceLimit::MetadataEntries => ProviderJsonlInventoryLimit::MetadataEntries,
                },
                maximum,
                observed,
            },
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
            SourceIoError::SqliteFinalization {
                primary,
                finalization,
            } => Self::SqliteFinalization {
                primary: Box::new(Self::from(*primary)),
                finalization: Box::new(Self::from(*finalization)),
            },
        }
    }
}
