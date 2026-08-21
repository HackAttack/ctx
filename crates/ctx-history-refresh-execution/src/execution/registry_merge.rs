use super::*;

pub(crate) fn build_merged_source_backed_registry(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<MergedSourceBackedRegistry> {
    build_merged_source_backed_registry_with_automatic_routes(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        &BTreeSet::new(),
        published_state,
    )
}

pub(super) fn build_merged_source_backed_registry_with_automatic_routes(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    admitted_automatic_routes: &BTreeSet<SourceRouteIdentity>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<MergedSourceBackedRegistry> {
    let PublishedSourceBackedState {
        verified_index: retained_generation,
        explicit_source_catalog: previous_explicit_source_catalog,
        catalog_route_bindings: previous_catalog_route_bindings,
        route_controls: previous_route_controls,
    } = published_state.open_published_state(data_root)?;
    let mut build =
        build_automatic_source_backed_registry_from_report(discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    let requested_catalog_route_bindings = explicit_source_catalog
        .map(|catalog| {
            catalog.register_routes_after_discovery_merge(
                data_root,
                retained_generation.as_ref(),
                &mut build,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let canonicalized_previous = previous_explicit_source_catalog
        .as_ref()
        .map(|catalog| {
            catalog.canonicalize_published_bindings(
                &previous_catalog_route_bindings,
                &build.registry,
                admitted_automatic_routes,
            )
        })
        .transpose()?;
    let reactivated_automatic_routes = canonicalized_previous
        .as_ref()
        .map(|canonicalized| canonicalized.transitioned_routes.clone())
        .unwrap_or_default();
    for (replacement, retired) in canonicalized_previous
        .as_ref()
        .map(|canonicalized| canonicalized.retirements.clone())
        .unwrap_or_default()
    {
        build
            .registry
            .retire_routes_after_success(&replacement, retired)?;
    }
    let previous_catalog_route_bindings = canonicalized_previous
        .map(|canonicalized| canonicalized.bindings)
        .unwrap_or(previous_catalog_route_bindings);
    let route_retirements = ExplicitSourceCatalogAuthority::replacement_route_retirements(
        previous_explicit_source_catalog
            .as_ref()
            .map(|catalog| (catalog, previous_catalog_route_bindings.as_slice())),
        explicit_source_catalog
            .map(|catalog| (catalog, requested_catalog_route_bindings.as_slice())),
    )?;
    for (replacement, retired) in route_retirements {
        build
            .registry
            .retire_routes_after_success(&replacement, retired)?;
    }
    let previous_provider_root_routes = retained_generation
        .as_ref()
        .map(|generation| {
            generation
                .manifest()
                .provider_roots()
                .iter()
                .flat_map(|root| root.routes().iter().cloned())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let base_route_ids = retained_generation
        .as_ref()
        .map(|generation| {
            generation
                .manifest()
                .source_routes()
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let current_provider_root_routes = build
        .registry
        .applied_provider_roots()
        .map(|(_, _, roots)| {
            roots
                .iter()
                .flat_map(|root| root.routes().iter().cloned())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut retired_provider_root_routes = previous_provider_root_routes
        .difference(&current_provider_root_routes)
        .cloned()
        .collect::<BTreeSet<_>>();
    // Naming an already automatic home changes that route from inferred to
    // configured authority without duplicating its physical source. Retire
    // only an automatic predecessor that is no longer executable in the
    // additive registry; distinct automatic peers remain selected.
    if !discovery.configured_provider_roots().is_empty() {
        let current_executable_routes = build
            .registry
            .executable_route_identities()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let configured_providers = discovery
            .configured_provider_roots()
            .iter()
            .map(|root| root.provider)
            .collect::<Vec<_>>();
        for route in build
            .registry
            .routes()
            .filter(|route| configured_providers.contains(&route.source.provider))
        {
            if let Ok(automatic) = automatic_source_backed_route_identity(&route.source) {
                if base_route_ids.contains(&automatic)
                    && !current_executable_routes.contains(&automatic)
                {
                    retired_provider_root_routes.insert(automatic);
                }
            }
        }
    }
    build
        .registry
        .set_provider_root_route_retirements(retired_provider_root_routes);
    Ok(MergedSourceBackedRegistry {
        build,
        reactivated_automatic_routes,
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog: explicit_source_catalog.cloned(),
        retained_generation,
        requested_catalog_route_bindings,
        previous_route_controls,
    })
}

pub(super) fn provider_root_publication_scope(
    requested: &SourceBackedRefreshScope,
    physical: &SourceBackedRefreshScope,
    registry: &ctx_history_capture::SourceBackedProviderRegistry,
    retained: Option<&VerifiedIndex>,
) -> SourceBackedRefreshScope {
    let changed = matches!(requested, SourceBackedRefreshScope::All)
        && retained.zip(registry.applied_provider_roots()).is_some_and(
            |(retained, (automatic, digest, roots))| {
                let manifest = retained.manifest();
                *automatic != manifest.automatic_provider_discovery()
                    || digest != manifest.provider_root_config_digest()
                    || roots != manifest.provider_roots()
            },
        );
    if changed {
        SourceBackedRefreshScope::All
    } else {
        physical.clone()
    }
}
