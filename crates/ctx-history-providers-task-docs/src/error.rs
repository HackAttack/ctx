use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ctx_history_source_io::SourceIoJsonlInventoryLimit,
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
}

pub(crate) type Result<T> = std::result::Result<T, CaptureError>;

impl From<ctx_history_source_io::SourceIoError> for CaptureError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        match error {
            ctx_history_source_io::SourceIoError::Io(error) => Self::Io(error),
            ctx_history_source_io::SourceIoError::Json(error) => Self::Json(error),
            ctx_history_source_io::SourceIoError::InvalidPayload(detail) => {
                Self::InvalidPayload(detail)
            }
            ctx_history_source_io::SourceIoError::InvalidProviderTranscriptPath {
                path,
                reason,
            } => Self::InvalidProviderTranscriptPath { path, reason },
            ctx_history_source_io::SourceIoError::SystemIo { operation, source } => {
                Self::SystemIo { operation, source }
            }
            ctx_history_source_io::SourceIoError::SystemInvariant(detail) => {
                Self::SystemInvariant(detail)
            }
            ctx_history_source_io::SourceIoError::SourceChangedDuringCapture => {
                Self::SourceChangedDuringCapture
            }
            ctx_history_source_io::SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => Self::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CaptureError;
    use ctx_history_source_io::{SourceIoError, SourceIoJsonlInventoryLimit};

    #[test]
    fn source_io_inventory_limit_error_preserves_structured_fields() {
        let error = CaptureError::from(SourceIoError::ProviderJsonlInventoryLimitExceeded {
            limit: SourceIoJsonlInventoryLimit::EligiblePaths,
            maximum: 3,
            observed: 4,
        });

        match error {
            CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => {
                assert_eq!(limit, SourceIoJsonlInventoryLimit::EligiblePaths);
                assert_eq!(maximum, 3);
                assert_eq!(observed, 4);
            }
            other => panic!("expected structured inventory limit error, got {other:?}"),
        }
    }
}
