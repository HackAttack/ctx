use std::path::Path;

use anyhow::Result;
use ctx_history_capture::{ProviderSource, SourceBackedRouteError, SourceBackedRouteErrorKind};

use ctx_history_core::CaptureProvider;

pub(crate) use ctx_history_refresh::{
    relocate_explicit_source, upsert_explicit_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceRelocationAuthority,
};

pub(crate) fn explicit_source_for_admission(
    path: &Path,
    provider: Option<CaptureProvider>,
    custom_history_jsonl: bool,
) -> Result<ProviderSource> {
    ctx_history_refresh::explicit_source_for_path(path, provider, custom_history_jsonl)
}

pub(crate) fn relocation_authority_for_import(
    data_root: &Path,
    old_path: &Path,
) -> Result<ExplicitSourceRelocationAuthority> {
    ctx_history_refresh::validate_explicit_relocation_source(old_path)?;
    crate::semantic::published_explicit_source_relocation_authority(data_root, old_path)?
        .ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unsupported,
                "relocation source is not the active exact catalog lineage/route",
            )
            .into()
        })
}
