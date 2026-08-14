//! Provider-owned task-document discovery, parsing, and Core projection.
//!
//! Capture owns route registration and publication; this pack owns no capture
//! dependency and contains the Cline/Roo task JSON, Continue, CodeBuddy, and
//! Rovo Dev provider implementations.

mod error;

pub use ctx_history_capture_model::{OutputObservationKind, OutputOutcome, OutputOutcomeMetadata};
pub use ctx_history_capture_runtime::{
    CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
    DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal, ObservedDocumentLeaf,
    ReplacementDocumentTree, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};
pub use ctx_history_provider_runtime::ProviderAdapterContext;
pub use ctx_history_source_discovery::{ProviderSource, ProviderSourceKind, ProviderSourceStatus};
pub(crate) use error::{CaptureError, Result};
pub(crate) const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
pub const CLINE_TASK_JSON_SOURCE_FORMAT: &str = "cline_task_directory_json";
pub const ROO_TASK_JSON_SOURCE_FORMAT: &str = "roo_task_directory_json";
pub const CODEBUDDY_SOURCE_FORMAT: &str = "codebuddy_history_json";
pub const CONTINUE_CLI_SOURCE_FORMAT: &str = "continue_cli_sessions_json";
pub const ROVODEV_SOURCE_FORMAT: &str = "rovodev_session_json_tree";

pub(crate) mod common {
    pub(crate) mod io {
        use crate::CaptureError;
        pub(crate) type ProviderSourceRoot =
            ctx_history_source_io::MappedProviderSourceRoot<CaptureError>;
        pub(crate) type ProviderSourceDirectory =
            ctx_history_source_io::MappedProviderSourceDirectory<CaptureError>;
        pub(crate) type OpenedProviderSourceFile =
            ctx_history_source_io::MappedOpenedProviderSourceFile<CaptureError>;
        pub(crate) type OpenedProviderSourcePath =
            ctx_history_source_io::MappedOpenedProviderSourcePath<CaptureError>;

        pub(crate) fn open_provider_source_path(
            path: &std::path::Path,
        ) -> Result<OpenedProviderSourcePath, CaptureError> {
            ctx_history_source_io::open_provider_source_path_mapped(path)
        }

        pub(crate) fn ensure_regular_provider_transcript_file(
            path: &std::path::Path,
        ) -> Result<(), CaptureError> {
            ctx_history_source_io::ensure_regular_provider_transcript_file_mapped(path)
        }

        pub(crate) fn ensure_provider_path_parents_are_not_symlinks(
            path: &std::path::Path,
        ) -> Result<(), CaptureError> {
            ctx_history_source_io::ensure_provider_path_parents_are_not_symlinks_mapped(path)
        }
    }
}

pub mod providers;

pub(crate) fn provider_safe_path_segment(value: &str) -> bool {
    ctx_history_source_io::provider_safe_path_segment(value)
}

pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub(crate) type ProviderLifecycleMarker<L, S, C> = std::marker::PhantomData<fn() -> (L, S, C)>;

#[cfg(test)]
pub(crate) mod test_support_paths {
    pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
        let temp_root = std::fs::canonicalize(std::env::temp_dir())?;
        tempfile::Builder::new()
            .prefix("ctx-history-providers-task-docs-")
            .tempdir_in(temp_root)
    }
}
