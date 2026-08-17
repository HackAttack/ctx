use super::*;
use std::collections::BTreeMap;

impl ExplicitSourceCatalogAuthority {
    #[cfg(test)]
    pub(crate) fn prepare_retained_discovery_report(
        &self,
        requested: Option<&Self>,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        self.prepare_retained_discovery_report_with_automatic_routes(requested, report, &[])
    }

    pub(crate) fn prepare_retained_discovery_report_with_automatic_routes(
        &self,
        requested: Option<&Self>,
        report: &mut DiscoveryReport,
        reactivated_automatic_sources: &[ProviderSource],
    ) -> Result<()> {
        let requested_keys = requested
            .map(Self::exact_route_keys)
            .transpose()?
            .unwrap_or_default();
        let entries = self
            .entries
            .iter()
            .map(|entry| Ok((entry.exact_route_key()?, entry)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(key, entry)| (!requested_keys.contains(&key)).then(|| entry.clone()))
            .collect();
        remove_automatic_routes_shadowed_by_snapshot(
            report,
            &ExplicitSourceCatalogSnapshot { entries },
            reactivated_automatic_sources,
        );
        Ok(())
    }

    pub(crate) fn prepare_discovery_report(
        &self,
        report: &mut DiscoveryReport,
        retained_registration_sources: &[ProviderSource],
    ) {
        remove_automatic_routes_shadowed_by_snapshot(
            report,
            &self.snapshot(),
            retained_registration_sources,
        );
    }

    pub(crate) fn has_codex_session_tree_entry(&self) -> Result<bool> {
        for entry in self.entries.iter().filter(|entry| entry.enabled) {
            if entry.provider()? == CaptureProvider::Codex
                && entry.route_metadata()?.source_format == "codex_session_jsonl_tree"
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn secondary_codex_registration_sources(
        &self,
        registry: &SourceBackedProviderRegistry,
    ) -> Result<Vec<ProviderSource>> {
        let mut sources = Vec::new();
        for entry in self.entries.iter().filter(|entry| entry.enabled) {
            if entry.provider()? != CaptureProvider::Codex
                || entry.route_metadata()?.source_format != "codex_session_jsonl_tree"
            {
                continue;
            }
            let certified_format = entry.certified_source_format()?;
            for route in registry.routes().filter(|route| {
                route.selection == Some(SourceBackedRouteSelection::Automatic)
                    && route.source.provider == CaptureProvider::Codex
                    && route.source.path != entry.path
                    && route.certified_source_format == certified_format
            }) {
                let Some(route_identity) = route.route_identity.as_ref() else {
                    continue;
                };
                let Some(registration_sources) =
                    registry.automatic_route_registration_sources(route_identity)
                else {
                    continue;
                };
                sources.extend(
                    registration_sources
                        .filter(|source| {
                            source.provider == CaptureProvider::Codex
                                && source.path == entry.path
                                && route_metadata(source.provider, source.source_format)
                                    .ok()
                                    .is_some_and(|metadata| {
                                        metadata.certified_source_format == certified_format
                                    })
                        })
                        .cloned(),
                );
            }
        }
        Ok(sources)
    }

    pub(crate) fn automatic_reactivation_retirements(
        &self,
        bindings: &[ExplicitSourceCatalogRouteBinding],
        build: &SourceBackedAutomaticRegistryBuild,
        reactivated_automatic_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
    ) -> Result<
        BTreeMap<
            ctx_history_index::SourceRouteIdentity,
            Vec<ctx_history_index::SourceRouteIdentity>,
        >,
    > {
        let bound_routes = self.bound_routes(bindings)?;
        let mut retirements = BTreeMap::new();
        for route in build.registry.routes().filter(|route| {
            route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route
                    .route_identity
                    .as_ref()
                    .is_some_and(|identity| reactivated_automatic_routes.contains(identity))
        }) {
            let route_identity = route
                .route_identity
                .as_ref()
                .expect("reactivated automatic routes carry identities")
                .clone();
            // A certified-missing automatic route is watchable but has no
            // executable replacement driver. It cannot retire an explicit
            // owner until a later discovery makes that same route executable.
            let Some(registration_sources) = build
                .registry
                .automatic_route_registration_sources(&route_identity)
            else {
                continue;
            };
            let registration_sources = registration_sources.collect::<Vec<_>>();
            let mut retired = Vec::new();
            for (entry, previous_route) in &bound_routes {
                let entry_provider = entry.provider()?;
                let entry_format = entry.certified_source_format()?;
                if entry.enabled
                    && entry_provider == route.source.provider
                    && entry_format == route.certified_source_format
                    && registration_sources.iter().any(|source| {
                        source.provider == entry_provider
                            && source.path == entry.path
                            && route_metadata(source.provider, source.source_format)
                                .ok()
                                .is_some_and(|metadata| {
                                    metadata.certified_source_format == entry_format
                                })
                    })
                    && previous_route != &route_identity
                {
                    retired.push(previous_route.clone());
                }
            }
            if !retired.is_empty() && retirements.insert(route_identity, retired).is_some() {
                bail!("reactivated automatic catalog contains duplicate route identities");
            }
        }
        Ok(retirements)
    }

    pub(crate) fn register_routes_after_discovery_merge(
        &self,
        data_root: &Path,
        base_generation: Option<&VerifiedIndex>,
        build: &mut SourceBackedAutomaticRegistryBuild,
    ) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
        let snapshot = self.snapshot();
        register_explicit_source_catalog_snapshot_routes(
            data_root,
            base_generation,
            build,
            &snapshot,
        )
    }

    fn exact_route_keys(&self) -> Result<BTreeSet<(String, String, PathBuf)>> {
        self.entries
            .iter()
            .map(CatalogEntry::exact_route_key)
            .collect()
    }
}

impl CatalogEntry {
    fn exact_route_key(&self) -> Result<(String, String, PathBuf)> {
        Ok((
            self.provider()?.as_str().to_owned(),
            self.certified_source_format()?.to_owned(),
            self.path.clone(),
        ))
    }
}
