use super::*;
use std::collections::BTreeMap;

impl ExplicitSourceCatalogAuthority {
    #[cfg(test)]
    pub(crate) fn prepare_retained_discovery_report(
        &self,
        requested: Option<&Self>,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        self.prepare_retained_discovery_report_with_automatic_routes(
            requested,
            report,
            &BTreeSet::new(),
        )
    }

    pub(crate) fn prepare_retained_discovery_report_with_automatic_routes(
        &self,
        requested: Option<&Self>,
        report: &mut DiscoveryReport,
        reactivated_automatic_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
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
            reactivated_automatic_routes,
        );
        Ok(())
    }

    pub(crate) fn prepare_discovery_report(
        &self,
        _data_root: &Path,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        let snapshot = self.snapshot();
        remove_automatic_routes_shadowed_by_snapshot(report, &snapshot, &BTreeSet::new());
        Ok(())
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
            let mut retired = Vec::new();
            for (entry, previous_route) in &bound_routes {
                if entry.enabled
                    && entry.provider()? == route.source.provider
                    && entry.path == route.source.path
                    && entry.certified_source_format()? == route.certified_source_format
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
