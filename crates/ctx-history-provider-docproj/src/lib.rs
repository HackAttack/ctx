pub use ctx_history_capture_model::{
    OutputOutcome, ProviderSource, ProviderSourceFailureKind, ProviderSourceKind,
    ProviderSourceStatus,
};
pub use ctx_history_provider_runtime::{
    invalid_route_error, CaptureError, ProviderAdapterContext, ProviderJsonlInventoryLimit, Result,
};
pub use ctx_history_source_io::{
    ProviderJsonlInventory, ProviderJsonlInventoryLimits, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
pub use provider_sources::{
    path_presence, resolve_warp_discovery_authority, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, EventFileCoordinates,
    EventFileGroup, EventFileInventory, EventFileInventoryError, EventFileLimits,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, OrdinaryFileObservation, PathPresence,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceRootBoundaryError,
    WarpDiscoveryUnavailable,
};

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
pub const AUGGIE_SESSION_JSON_SOURCE_FORMAT: &str = "auggie_session_json";
pub const NANOCLAW_SOURCE_FORMAT: &str = "nanoclaw_project";
pub const OPENHANDS_FILE_EVENTS_SOURCE_FORMAT: &str = "openhands_file_events";

pub mod provider_sources;

pub(crate) mod common {
    pub(crate) mod io {
        pub(crate) use ctx_history_provider_runtime::source_io::*;
    }
}

pub(crate) mod provider {
    pub(crate) use ctx_history_source_io::provider_safe_path_segment;

    pub(crate) mod providers {
        pub(crate) use crate::providers::{auggie, nanoclaw, openhands};
    }

    pub(crate) mod sqlite {
        pub(crate) use ctx_history_provider_runtime::*;
    }

    pub(crate) mod source_backed {
        pub(crate) use ctx_history_capture_runtime::{
            SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
            SourceBackedRecordRejectionDrafts, SourceBackedRouteError, SourceBackedRouteErrorKind,
            SourceBackedRouteResult,
        };
        pub(crate) use ctx_history_provider_runtime::{
            combine_primary_and_cleanup_route_errors, invalid_route_error as route_error,
        };

        pub(crate) mod family {
            pub(crate) mod document {
                pub(crate) use ctx_history_capture_runtime::{
                    CompleteDocumentTree, DocumentLeafExecutionPolicy, DocumentLeafFingerprint,
                    DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
                };
                pub(crate) use ctx_history_provider_runtime::ProviderChangedDocumentSink as ChangedDocumentSink;
            }
        }
    }
}

pub mod providers;

#[cfg(test)]
mod test_support_paths;
