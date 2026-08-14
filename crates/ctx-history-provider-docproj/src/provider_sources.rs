pub use ctx_history_source_discovery::{
    path_presence, resolve_warp_discovery_authority, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, LingmaDiscoveryUnavailable,
    LingmaInventorySelector, OrdinaryFileObservation, PathPresence, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSourceRootBoundaryError, WarpDiscoveryUnavailable,
};
pub use ctx_history_source_io::{
    EventFileCoordinates, EventFileGroup, EventFileInventory, EventFileInventoryError,
    EventFileLimits,
};
ctx_history_source_io::define_mapped_ordinary_io_compat!(crate::CaptureError);
#[cfg(test)]
pub use ctx_history_source_io::count_event_file_io;
#[cfg(test)]
pub use ctx_history_source_sqlite::{
    fail_next_opened_snapshot_cleanup_for_test,
    fail_next_private_sqlite_staging_operation_for_test, SqliteSourceStagingOperationForTest,
};
pub use ctx_history_source_sqlite::{
    open_private_sqlite_staging_file, open_root_handle_sqlite_source_snapshot,
    retain_sqlite_source_directory_authority, SqliteSourceAccessError,
    SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot, SqliteSourceStagingFile,
    SqliteSourceStagingReader,
};
