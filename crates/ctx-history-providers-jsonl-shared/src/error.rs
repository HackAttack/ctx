use std::path::PathBuf;

use ctx_history_capture_model::ProviderSourceFailureKind;
use thiserror::Error;

pub type ProviderJsonlInventoryLimit = ctx_history_source_io::ProviderJsonlInventoryLimit;

/// Shared-provider JSONL adapters currently keep an explicit local error policy
/// that mirrors the JSONL-relevant capture surface. This preserves one
/// authority for provider-local behavior today; runtime composition will fold
/// the overlap into `ctx-history-provider-runtime` instead of introducing a
/// third policy layer here.
#[derive(Debug, Error)]
pub enum CaptureError {
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
    #[error("{provider} source {path:?} failed ({kind}): {detail}")]
    ProviderSource {
        provider: &'static str,
        path: PathBuf,
        kind: ProviderSourceFailureKind,
        detail: String,
    },
    #[error("provider cursor changed during bounded capture")]
    ProviderCursorConflict,
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
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => Self::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            },
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
        }
    }
}

impl ctx_history_jsonl::JsonlFamilyError for CaptureError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }
    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }
    fn worker_panicked(worker: &'static str) -> Self {
        Self::WorkerPanicked(worker)
    }
    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }
    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::SystemIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
    fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChangedDuringCapture)
            || matches!(self, Self::InvalidProviderTranscriptPath { reason, .. } if *reason == "provider source changed while its authority handle was retained")
    }
    fn is_resource_unavailable(&self) -> bool {
        matches!(self, Self::Io(_) | Self::SystemIo { .. }) && !self.is_not_found()
    }
    fn is_internal(&self) -> bool {
        matches!(self, Self::SystemInvariant(_) | Self::WorkerPanicked(_))
    }
    fn is_ignorable_membership_entry(&self) -> bool {
        crate::common::io::is_symlink_source_rejection(self)
            || crate::common::io::is_non_regular_source_rejection(self)
    }
}
