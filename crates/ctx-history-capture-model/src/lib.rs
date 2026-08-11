//! Provider-neutral capture contracts and value objects.
//!
//! This crate owns no source access, discovery execution, provider implementation,
//! repository evidence, refresh publication, or runtime policy.

mod identity;
mod import;
mod output;
mod record;
mod source;

pub use identity::{fnv1a64, stable_capture_uuid};
pub use import::{
    CatalogSummary, ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult,
};
pub use output::{OutputObservationKind, OutputOutcome, OutputOutcomeMetadata};
pub use record::RecordDigest;
pub use source::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceFailureKind,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
