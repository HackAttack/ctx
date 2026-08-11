mod context;
mod discovery;
mod lingma;
mod probes;
mod reasons;
mod resolvers;
mod selectors;
mod specs;
mod types;
mod warp;

pub use context::{
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, DISCOVERY_ENV_ALLOWLIST,
};
#[cfg(test)]
pub(crate) use ctx_history_source_io::count_event_file_io;
#[cfg(test)]
pub(crate) use ctx_history_source_io::forbid_ordinary_file_content_open;
pub use ctx_history_source_io::OrdinaryFileObservation;
pub(crate) use ctx_history_source_io::{
    EventFileCoordinates, EventFileGroup, EventFileInventory, EventFileInventoryError,
    EventFileLimits,
};
pub use discovery::{
    discover_provider_sources, discover_provider_sources_for_provider,
    discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_context_and_work_budget,
    discover_provider_sources_with_projects, provider_source_for_path,
    validate_provider_source_roots_outside_data_root, ProviderSourceRootBoundaryError,
};
pub use lingma::{
    discover_lingma_inventory_with_authority, resolve_lingma_discovery_authority,
    DiscoveredLingmaDatabase, LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
};

pub(crate) fn open_ordinary_file_without_following(
    path: &std::path::Path,
) -> crate::Result<std::fs::File> {
    ctx_history_source_io::open_ordinary_file_without_following(path).map_err(Into::into)
}

pub fn observe_ordinary_file(
    path: impl AsRef<std::path::Path>,
) -> crate::Result<OrdinaryFileObservation> {
    ctx_history_source_io::observe_ordinary_file(path).map_err(Into::into)
}
pub(crate) use crate::provider::sqlite::{
    sqlite_retry_decision, SqliteLogicalSnapshot, SqliteRetryDecision,
};
#[cfg(test)]
pub(crate) use ctx_history_source_io::{
    fail_next_opened_snapshot_cleanup_for_test, SqliteSourceSnapshotCounters,
};
pub(crate) use ctx_history_source_io::{
    open_root_handle_sqlite_source_snapshot, open_root_handle_sqlite_source_snapshot_with_limits,
    resource_exhaustion_io_error, retain_sqlite_source_directory_authority,
    rusqlite_busy_or_locked, rusqlite_resource_failure, SqliteArtifactKind, SqliteCleanupStatus,
    SqliteFailurePhase, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
    SqliteSourceEvidence, SqliteSourceProgressError, SqliteSourceReadSnapshot,
    SqliteSourceSnapshotLimits,
};
pub(crate) use resolvers::PathPresence;
pub(crate) use resolvers::{
    path_presence, CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError,
};
pub use specs::{provider_source_spec, provider_source_specs, HERMES_STATE_DB_UNSUPPORTED_REASON};
pub use types::{
    provider_source_status_reason, DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport,
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
pub use warp::{
    discover_warp_sources_with_authority, resolve_warp_discovery_authority, DiscoveredWarpSource,
    WarpDiscoveryUnavailable, WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel,
    WarpTerminalSurface,
};

#[cfg(test)]
mod tests;
