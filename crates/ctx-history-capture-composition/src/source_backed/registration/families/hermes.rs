use std::path::Path;

use ctx_history_core::SourceAnchor;
use ctx_history_provider_hermes::registration::{
    hermes_automatic_registration, hermes_explicit_registration,
};

use super::*;
use crate::provider::source_backed::family::document::{
    install_hermes_registration, CaptureDocumentLifecycle, CaptureDocumentSpool,
};

pub(super) fn register_hermes_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration = hermes_automatic_registration::<
        CaptureDocumentLifecycle,
        CaptureDocumentSpool,
    >(source, selection, data_root)
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_hermes_registration(registry, registration)
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
    install_hermes_registration(registry, registration)
}
