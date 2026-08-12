//! Provider-neutral capture contracts, state, and value objects.
//!
//! This crate owns no source access, discovery execution, provider implementation,
//! repository evidence, refresh publication, or runtime policy.

pub mod ctx_retrieval;
mod exact_json;
pub mod file_touches;
mod identity;
mod import;
pub mod normalization;
mod output;
mod progress;
mod record;
mod route;
mod source;
pub mod time;
pub mod tool_input;

pub use exact_json::{
    exact_bounded_string_alias, exact_json_value, raw_object_keys_are_unique, ExactJsonStringAlias,
};
pub use identity::{fnv1a64, stable_capture_uuid};
pub use import::{
    push_provider_import_failure, CatalogSummary, ProviderImportFailure, ProviderImportSummary,
    ProviderImportWorkResult,
};
pub use output::{OutputObservationKind, OutputOutcome, OutputOutcomeMetadata};
pub use progress::{
    source_level_progress, AttemptHistoryProgress, AttemptHistoryProgressSnapshot,
    CoreRecordBatchProgress, CoreRecordProgress, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedDetailedRefreshProgress,
    SourceBackedRecordProgressDelta, SourceBackedRefreshProgress, SourceRecordProgress,
    SourceRecordProgressSnapshot,
};
pub use record::RecordDigest;
pub use route::{SourceRouteIdentity, SourceRouteIdentityError};
pub use source::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceFailureKind,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
