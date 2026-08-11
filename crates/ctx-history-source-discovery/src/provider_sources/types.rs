pub use ctx_history_capture_model::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
use ctx_history_core::CaptureProvider;

/// Applies provider discovery policy to a captured source observation.
pub fn provider_source_status_reason(
    source: &ProviderSource,
) -> Option<ProviderSourceStatusReason> {
    match (
        source.provider,
        source.status,
        source.source_kind,
        source.import_support,
    ) {
        (
            CaptureProvider::Trae,
            ProviderSourceStatus::Unknown,
            ProviderSourceKind::DetectionOnly,
            ProviderImportSupport::Unsupported,
        ) => Some(ProviderSourceStatusReason::BlockedAuthOrEncryption),
        _ => None,
    }
}
