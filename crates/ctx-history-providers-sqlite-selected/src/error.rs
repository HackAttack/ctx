use std::path::PathBuf;

use thiserror::Error;

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
}

pub type Result<T> = std::result::Result<T, CaptureError>;

impl From<ctx_history_source_io::SourceIoError> for CaptureError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        use ctx_history_source_io::SourceIoError;

        match error {
            SourceIoError::Io(error) => Self::Io(error),
            SourceIoError::Json(error) => Self::Json(error),
            SourceIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SourceIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SourceIoError::ProviderJsonlInventoryLimitExceeded { .. } => {
                Self::SystemInvariant("SQLite provider reached a JSONL inventory error")
            }
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
        }
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for CaptureError {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        use ctx_history_source_sqlite::SqliteIoError;

        match error {
            SqliteIoError::Io(error) => Self::Io(error),
            SqliteIoError::Sqlite(error) => Self::Sqlite(error),
            SqliteIoError::Json(error) => Self::Json(error),
            SqliteIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SqliteIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SqliteIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SqliteIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SqliteIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
            SqliteIoError::SqliteFinalization {
                primary,
                finalization,
            } => Self::SqliteFinalization {
                primary: Box::new(Self::from(*primary)),
                finalization: Box::new(Self::from(*finalization)),
            },
        }
    }
}
