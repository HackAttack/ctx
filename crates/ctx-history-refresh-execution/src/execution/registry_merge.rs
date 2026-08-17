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
    mut report: DiscoveryReport,
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
    let authenticate_requested_codex = explicit_source_catalog
        .map(ExplicitSourceCatalogAuthority::has_codex_session_tree_entry)
        .transpose()?
        .unwrap_or(false);
    let authenticated_automatic_build = ((previous_explicit_source_catalog.is_some()
        && !admitted_automatic_routes.is_empty())
        || authenticate_requested_codex)
        .then(|| {
            build_automatic_source_backed_registry_from_report(discovery, data_root, report.clone())
        });
    let mut reactivated_automatic_sources = Vec::new();
    if previous_explicit_source_catalog.is_some() && !admitted_automatic_routes.is_empty() {
        let authenticated = authenticated_automatic_build
            .as_ref()
            .expect("automatic reactivation requested an authenticated registry");
        for route in admitted_automatic_routes {
            if let Some(sources) = authenticated
                .registry
                .automatic_route_registration_sources(route)
            {
                reactivated_automatic_sources.extend(sources.cloned());
            }
        }
    }
    let secondary_codex_registration_sources = explicit_source_catalog
        .filter(|_| authenticate_requested_codex)
        .map(|catalog| {
            catalog.secondary_codex_registration_sources(
                &authenticated_automatic_build
                    .as_ref()
                    .expect("explicit request built an authenticated automatic registry")
                    .registry,
            )
        })
        .transpose()?
        .unwrap_or_default();
    // A request overlay is not the whole durable explicit catalog. Keep every
    // unmatched retained explicit owner out of automatic discovery so those
    // base routes remain carried rather than being re-scanned under a new
    // automatic identity. An exact automatic watcher admission may reclaim
    // only the exact registration roots authenticated by its grouped route;
    // relocation deliberately preserves lineage while changing the path.
    if let Some(catalog) = previous_explicit_source_catalog.as_ref() {
        catalog.prepare_retained_discovery_report_with_automatic_routes(
            explicit_source_catalog,
            &mut report,
            &reactivated_automatic_sources,
        )?;
    }
    if let Some(catalog) = explicit_source_catalog {
        // Codex combines sessions and archived_sessions into one authenticated
        // automatic route. Keep only an exact requested secondary root in the
        // automatic registration so the shared generation coordinator can
        // transfer that root without dropping the route's primary root.
        catalog.prepare_discovery_report(&mut report, &secondary_codex_registration_sources);
    }
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
    let automatic_reactivation_retirements = previous_explicit_source_catalog
        .as_ref()
        .map(|catalog| {
            catalog.automatic_reactivation_retirements(
                &previous_catalog_route_bindings,
                &build,
                admitted_automatic_routes,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let reactivated_automatic_routes = automatic_reactivation_retirements
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    for (replacement, retired) in automatic_reactivation_retirements {
        build
            .registry
            .retire_routes_after_success(&replacement, retired)?;
    }
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
