use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{SourceAnchor, TypedKey};
use ctx_history_providers_sqlite_inventory::registration::{
    astrbot_registration, crush_registration, hermes_explicit_registration, lingma_registration,
    shelley_registration,
};

use super::*;
use crate::provider::source_backed::family::document::{
    install_sqlite_inventory_registration, CaptureDocumentLifecycle, CaptureDocumentSpool,
};

pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    install_sqlite_inventory_registration(
        registry,
        astrbot_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source, selection, data_root, discovery,
        ),
    )
}

pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory: Arc<I>,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    install_sqlite_inventory_registration(
        registry,
        crush_registration::<I, CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source, selection, data_root, inventory,
        ),
    )
}

pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration = lingma_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
        source,
        selection,
        data_root,
        authority_key,
        databases,
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_sqlite_inventory_registration(registry, registration)
}

pub fn register_shelley_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration = shelley_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
        source, data_root, exact_cwd,
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_sqlite_inventory_registration(registry, registration)
}

pub fn register_hermes_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    anchor: SourceAnchor,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration =
        hermes_explicit_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source, data_root, anchor,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_sqlite_inventory_registration(registry, registration)
}
