use super::*;

mod catalog_witness;
use catalog_witness::retained_catalog_witness;

pub(super) struct SourceBackedRefreshPlan<'a> {
    pub(super) explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) reconciliation_demand: SourceBackedReconciliationDemand,
    pub(super) scope: SourceBackedRefreshScope,
    pub(super) covered_route_ids: BTreeSet<SourceRouteIdentity>,
    pub(super) covered_publication: SourceBackedRefreshCoveredPublication,
}

pub(crate) struct RetainedPublishedState<'a> {
    pub(crate) journal: &'a dyn RefreshJournal,
}

impl PublishedSourceBackedStatePort for RetainedPublishedState<'_> {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState> {
        let verified_index = open_published_generation(data_root, self.journal)?;
        let (explicit_source_catalog, catalog_route_bindings) =
            retained_catalog_witness(verified_index.as_ref())?;
        let route_controls = verified_index
            .as_ref()
            .and_then(|index| SourceBackedPublicationMetadata::decode(index).ok())
            .map(|metadata| metadata.route_controls)
            .unwrap_or_default();
        Ok(PublishedSourceBackedState {
            verified_index,
            explicit_source_catalog,
            catalog_route_bindings,
            route_controls,
        })
    }
}

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &CoreRefreshEngine,
    plan: SourceBackedRefreshPlan<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let discovery_context = coordinator.runtime.discovery_context(data_root)?;
    let published_state = RetainedPublishedState {
        journal: coordinator.journal.as_ref(),
    };
    let report_progress = |update: PhysicalRefreshProgressUpdate| {
        coordinator.persist_progress(
            data_root,
            request_id,
            SourceBackedRefreshProgressUpdate {
                phase: update.phase,
                completed_sources: update.completed_sources,
                total_sources: update.total_sources,
                total_sources_known: update.total_sources_known,
                current_source: update.current_source,
                completed_records: update.completed_records,
                completed_bytes: update.completed_bytes,
                providers: update.providers,
                processed_sessions: update.processed_sessions,
                processed_messages: update.processed_messages,
                processed_tool_calls: update.processed_tool_calls,
                processed_bytes: update.processed_bytes,
                elapsed_millis: update.elapsed_millis,
                current_source_progress: update.current_source_progress,
            },
        )
    };
    executor.refresh(
        SourceBackedRefreshExecution::new(
            data_root,
            &index_root,
            request_id,
            plan.operation,
            plan.explicit_source_catalog,
            plan.scope,
            plan.covered_route_ids,
            plan.covered_publication,
            &discovery_context,
            &published_state,
            &report_progress,
        )
        .with_reconciliation_demand(plan.reconciliation_demand),
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let journal = TestRefreshJournal::default();
    refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        "test-refresh",
        SourceBackedRefreshOperation::Refresh,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        &SourceBackedRefreshCoveredPublication::default(),
        &journal,
        report_progress,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_all_provider_sources_route_local(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: SourceBackedRefreshOperation,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    covered_publication: &SourceBackedRefreshCoveredPublication,
    journal: &dyn RefreshJournal,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let published_state = RetainedPublishedState { journal };
    ctx_history_refresh_execution::refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        covered_publication,
        &published_state,
        report_progress,
    )
}

/// Captures the logical caller's admission fence over the current automatic
/// route catalog. Missing observation tokens are retained explicitly so
/// coverage evaluation fails closed instead of treating silence as freshness.
pub(super) fn source_backed_route_admission_fence(
    discovery: &DiscoveryContext,
    journal: &dyn RefreshJournal,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>> {
    source_backed_route_observation_fence(
        discovery,
        journal,
        data_root,
        explicit_source_catalog,
        None,
    )
}

/// Samples only the exact routes that can contribute to one publication
/// coverage certificate. Requested routes absent from the current catalog are
/// retained with an indeterminate observation so certification fails closed.
pub(super) fn source_backed_requested_route_observation_fence(
    discovery: &DiscoveryContext,
    journal: &dyn RefreshJournal,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    requested_routes: &BTreeSet<SourceRouteIdentity>,
) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>> {
    source_backed_route_observation_fence(
        discovery,
        journal,
        data_root,
        explicit_source_catalog,
        Some(requested_routes),
    )
}

fn source_backed_route_observation_fence(
    discovery: &DiscoveryContext,
    journal: &dyn RefreshJournal,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    requested_routes: Option<&BTreeSet<SourceRouteIdentity>>,
) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>> {
    let discovery = discovery.clone().with_data_root(data_root);
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
        .context("validate provider roots before admitting source refresh demand")?;
    let published_state = RetainedPublishedState { journal };
    let catalog = ctx_history_refresh_execution::source_backed_watch_catalog_from_report(
        &discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        &published_state,
    )?;
    Ok(match requested_routes {
        Some(requested_routes) => {
            source_backed_requested_route_observations(&catalog, requested_routes)
        }
        None => catalog
            .route_ids()
            .cloned()
            .map(|route| {
                let observation = catalog.certify_route_observation(&route);
                (route, observation)
            })
            .collect(),
    })
}
