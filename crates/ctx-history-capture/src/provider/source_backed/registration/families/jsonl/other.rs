use super::*;

/// Registers Cursor's thin adapter over the shared certified-append JSONL
/// lifecycle.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        crate::provider::providers::cursor::cursor_jsonl_adapter(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_junie_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_provider_family_driver(
        ctx_history_providers_jsonl_shared::adapters::junie::<
            crate::provider::source_backed::family::jsonl::CaptureProviderJsonlRuntime,
        >(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_kimi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_provider_family_driver(
        ctx_history_providers_jsonl_shared::adapters::kimi::<
            crate::provider::source_backed::family::jsonl::CaptureProviderJsonlRuntime,
        >(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        scan_mistral_vibe_source_backed(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_openclaw_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = crate::provider_source_for_path(CaptureProvider::OpenClaw, source.path.clone());
    if selected.status == ProviderSourceStatus::Unsupported {
        return Err(invalid_route(
            source.provider,
            selected
                .unsupported_reason
                .unwrap_or("unsupported OpenClaw history format"),
        ));
    }
    let driver = crate::provider::source_backed::family::jsonl::jsonl_provider_family_driver(
        ctx_history_providers_jsonl_shared::adapters::openclaw::<
            crate::provider::source_backed::family::jsonl::CaptureProviderJsonlRuntime,
        >(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_mux_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        mux_jsonl_adapter(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_pi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let (root, adapter) = ctx_history_providers_jsonl_shared::adapters::pi::<
        crate::provider::source_backed::family::jsonl::CaptureProviderJsonlRuntime,
    >(
        source.path.clone(),
        matches!(selection, SourceBackedRouteSelection::Automatic),
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver =
        crate::provider::source_backed::family::jsonl::jsonl_provider_family_driver(adapter, root);
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
/// Registers one caller-owned Custom History JSONL route. The path is only a
/// resolver location; `catalog_lineage` remains the durable source identity.
pub fn register_custom_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let adapter = ctx_history_providers_jsonl_shared::adapters::custom_history::<
        crate::provider::source_backed::family::jsonl::CaptureProviderJsonlRuntime,
    >(source.path.clone(), catalog_lineage)
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = crate::provider::source_backed::family::jsonl::jsonl_provider_family_driver(
        adapter,
        source.path.clone(),
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}
