//! Kiro's provider-owned NativePath ingestion leaf.
//!
//! The provider entry point below routes solely to its NativePath driver.
//! There is no alternate producer, projector, coordinator, or runtime fallback.

mod event;
mod history;
pub(crate) mod native_path;

pub fn kiro_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
) -> ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    native_path::source_backed_driver::<B>(
        ctx_history_core::CaptureProvider::KiroCli.as_str(),
        crate::KIRO_SQLITE_SOURCE_FORMAT,
        source_path,
        data_root,
    )
}
