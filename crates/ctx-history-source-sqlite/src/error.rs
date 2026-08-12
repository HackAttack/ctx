use std::path::PathBuf;

use ctx_history_source_io::SourceIoError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqliteIoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
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
        primary: Box<SqliteIoError>,
        finalization: Box<SqliteIoError>,
    },
}

impl From<SourceIoError> for SqliteIoError {
    fn from(error: SourceIoError) -> Self {
        match error {
            SourceIoError::Io(error) => Self::Io(error),
            SourceIoError::Json(error) => Self::Json(error),
            SourceIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SourceIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SourceIoError::ProviderJsonlInventoryLimitExceeded { .. } => {
                Self::SystemInvariant("SQLite source access reached a JSONL inventory error")
            }
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
        }
    }
}

pub type Result<T> = std::result::Result<T, SqliteIoError>;
