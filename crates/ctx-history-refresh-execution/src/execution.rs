use super::*;

pub(super) struct MergedSourceBackedRegistry {
    pub(super) build: ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    reactivated_automatic_routes: BTreeSet<SourceRouteIdentity>,
    previous_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    previous_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    retained_generation: Option<VerifiedIndex>,
    requested_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    previous_route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

enum SourceBackedInventoryDisposition {
    AuthoritativeContent,
    AuthoritativeEmpty(Vec<SourceBackedZeroSourceAuthority>),
    UnsupportedOrUnavailable(ZeroSourcePublicationBlocked),
}

#[derive(Debug)]
struct ExactMemberFallbackRequired;

impl std::fmt::Display for ExactMemberFallbackRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("exact member requires registered-route family reconciliation")
    }
}

impl std::error::Error for ExactMemberFallbackRequired {}

#[derive(Clone)]
struct CatalogRefreshAdmission {
    report: DiscoveryReport,
    exact_members: bool,
}

struct PreopenedPublishedState(Mutex<Option<PublishedSourceBackedState>>);

impl PublishedSourceBackedStatePort for PreopenedPublishedState {
    fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
        self.0
            .lock()
            .map_err(|_| anyhow!("preopened published source state lock was poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("preopened published source state was already consumed"))
    }
}

pub(super) fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let catalog_admission = catalog_refresh_admission(&execution);
    let mut family_fallback = execution.clone();
    match execute_capture_owned_refresh_once(execution, catalog_admission.clone()) {
        Err(error)
            if error
                .downcast_ref::<ExactMemberFallbackRequired>()
                .is_some() =>
        {
            family_fallback.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive;
            family_fallback
                .route_worksets
                .values_mut()
                .for_each(|workset| *workset = SourceBackedRefreshWorkset::Exhaustive);
            execute_capture_owned_refresh_once(
                family_fallback,
                catalog_admission.map(|mut admission| {
                    admission.exact_members = false;
                    admission
                }),
            )
        }
        result => result,
    }
}

fn execute_capture_owned_refresh_once(
    execution: SourceBackedRefreshExecution<'_>,
    catalog_admission: Option<CatalogRefreshAdmission>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery_context = execution.discovery_context;
    let reconciliation_demand = execution.reconciliation_demand;
    let route_worksets = execution
        .route_worksets
        .iter()
        .filter_map(|(route, workset)| match workset {
            SourceBackedRefreshWorkset::Members(paths) => Some((route.clone(), paths.clone())),
            SourceBackedRefreshWorkset::Exhaustive => None,
        })
        .collect::<BTreeMap<_, _>>();
    execute_capture_owned_refresh_with(
        execution,
        discovery_context,
        catalog_admission
            .as_ref()
            .map(|admission| admission.report.clone()),
        move |discovery,
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
              published_state,
              report_progress| {
            refresh_all_provider_sources_route_local_with_reconciliation(
                discovery,
                report,
                discovery_duration,
                request_id,
                operation,
                reconciliation_demand,
                catalog_admission
                    .as_ref()
                    .is_some_and(|admission| admission.exact_members),
                &route_worksets,
                data_root,
                index_root,
                explicit_source_catalog,
                scope,
                covered_route_ids,
                covered_publication,
                published_state,
                report_progress,
            )
        },
    )
}

#[doc(hidden)]
pub fn execute_capture_owned_refresh_with<Refresh>(
    execution: SourceBackedRefreshExecution<'_>,
    discovery: &DiscoveryContext,
    catalog_report: Option<DiscoveryReport>,
    refresh_all: Refresh,
) -> Result<SourceBackedRefreshPublication>
where
    Refresh: FnOnce(
        &DiscoveryContext,
        DiscoveryReport,
        StdDuration,
        &str,
        RefreshOperation,
        &Path,
        &Path,
        Option<&ExplicitSourceCatalogAuthority>,
        SourceBackedRefreshScope,
        &BTreeSet<SourceRouteIdentity>,
        &SourceBackedRefreshCoveredPublication,
        &dyn PublishedSourceBackedStatePort,
        &mut dyn FnMut(CaptureSourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    let discovery = discovery.clone().with_data_root(execution.data_root);
    let published_state = execution
        .published_state
        .open_published_state(execution.data_root)?;
    let published_state = PreopenedPublishedState(Mutex::new(Some(published_state)));
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = catalog_report.unwrap_or_else(|| {
        discover_provider_sources_with_context_and_work_budget(&discovery, work_budget)
    });
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(execution.data_root, report.sources.iter())
        .context("validate provider roots before source-refresh state writes")?;
    if let Some(authority) = execution.explicit_source_catalog {
        authority
            .validate_source_roots(execution.data_root)
            .context(
                "validate requested explicit provider roots before source-refresh state writes",
            )?;
    }
    let mut report_progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        let progress = update.progress;
        execution
            .report_history_progress_with_total_state(
                progress.phase,
                progress.completed_sources,
                progress.total_sources,
                true,
                progress.current_source,
                progress.completed_records,
                progress.completed_bytes,
                update
                    .current_source_progress
                    .map(SourceBackedCurrentSourceProgress::from_capture),
                progress
                    .providers
                    .into_iter()
                    .map(|provider| provider.as_str().to_owned())
                    .collect(),
                progress.processed_sessions,
                progress.processed_messages,
                progress.processed_tool_calls,
                progress.processed_bytes,
                Some(u64::try_from(progress.elapsed.as_millis()).unwrap_or(u64::MAX)),
                update
                    .exact_scan_progress
                    .map(|exact| SourceBackedExactScanProgress {
                        total_bytes: exact.total_bytes,
                        completed_bytes: exact.completed_bytes,
                    }),
            )
            .map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    format!("persist daemon source-backed refresh progress: {error:#}"),
                )
            })
    };
    refresh_all(
        &discovery,
        report,
        discovery_duration,
        execution.request_id,
        execution.operation,
        execution.data_root,
        execution.index_root,
        execution.explicit_source_catalog,
        execution.scope.clone(),
        &execution.covered_route_ids,
        &execution.covered_publication,
        &published_state,
        &mut report_progress,
    )
}

fn catalog_refresh_admission(
    execution: &SourceBackedRefreshExecution<'_>,
) -> Option<CatalogRefreshAdmission> {
    if execution.operation != RefreshOperation::Refresh
        || execution.explicit_source_catalog.is_some()
    {
        return None;
    }
    let SourceBackedRefreshScope::Exact(routes) = &execution.scope else {
        return None;
    };
    let catalog = execution.watch_catalog.as_ref()?;
    let exact_member_report = (execution.reconciliation_demand
        == SourceBackedReconciliationDemand::Incremental)
        .then(|| {
            execution
                .route_worksets
                .iter()
                .map(|(route, workset)| match workset {
                    SourceBackedRefreshWorkset::Members(members) => {
                        Some((route.clone(), members.clone()))
                    }
                    SourceBackedRefreshWorkset::Exhaustive => None,
                })
                .collect::<Option<BTreeMap<_, _>>>()
                .and_then(|worksets| catalog.exact_member_discovery_report(routes, &worksets))
        })
        .flatten();
    if let Some(report) = exact_member_report {
        return Some(CatalogRefreshAdmission {
            report,
            exact_members: true,
        });
    }
    Some(CatalogRefreshAdmission {
        report: catalog.route_discovery_report(routes)?,
        exact_members: false,
    })
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn refresh_all_provider_sources_route_local(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    covered_publication: &SourceBackedRefreshCoveredPublication,
    published_state: &dyn PublishedSourceBackedStatePort,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    refresh_all_provider_sources_route_local_with_reconciliation(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        SourceBackedReconciliationDemand::Exhaustive,
        false,
        &BTreeMap::new(),
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        covered_publication,
        published_state,
        report_progress,
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn refresh_all_provider_sources_route_local_with_worksets(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    reconciliation_demand: SourceBackedReconciliationDemand,
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    covered_publication: &SourceBackedRefreshCoveredPublication,
    published_state: &dyn PublishedSourceBackedStatePort,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    refresh_all_provider_sources_route_local_with_reconciliation(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        reconciliation_demand,
        false,
        route_worksets,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        covered_publication,
        published_state,
        report_progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn refresh_all_provider_sources_route_local_with_reconciliation(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    reconciliation_demand: SourceBackedReconciliationDemand,
    exact_catalog_members: bool,
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    covered_publication: &SourceBackedRefreshCoveredPublication,
    published_state: &dyn PublishedSourceBackedStatePort,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let no_admitted_automatic_routes = BTreeSet::new();
    let admitted_automatic_routes = match (&scope, operation, explicit_source_catalog) {
        (SourceBackedRefreshScope::Exact(routes), RefreshOperation::Refresh, None) => routes,
        _ => &no_admitted_automatic_routes,
    };
    let MergedSourceBackedRegistry {
        mut build,
        reactivated_automatic_routes,
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog,
        retained_generation,
        requested_catalog_route_bindings,
        previous_route_controls,
    } = build_merged_source_backed_registry_with_automatic_routes(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        admitted_automatic_routes,
        published_state,
    )?;
    // A newly reactivated automatic identity has no same-route base state
    // from which an incremental member scan could carry the unvisited source
    // family. Promote only those ownership transitions to exhaustive route
    // work; ordinary watcher appends retain their member worksets.
    let route_worksets = route_worksets
        .iter()
        .filter(|(route, _)| !reactivated_automatic_routes.contains(*route))
        .map(|(route, members)| (route.clone(), members.clone()))
        .collect::<BTreeMap<_, _>>();
    if scope == SourceBackedRefreshScope::All
        && reconciliation_demand == SourceBackedReconciliationDemand::Exhaustive
    {
        register_automatic_hermes_profile_rename_retirements(
            &mut build,
            retained_generation.as_ref(),
            &previous_catalog_route_bindings,
            &previous_route_controls,
        )?;
    }
    let registry_failures = if matches!(scope, SourceBackedRefreshScope::All) {
        reject_blocking_automatic_registry_issues(&build.issues)?;
        automatic_registry_route_failures(&build.issues, retained_generation.as_ref())?
    } else {
        Vec::new()
    };
    let route_less_blockers =
        automatic_registry_route_less_blockers(&build.issues, &registry_failures);
    let previous_nonempty_routes = retained_generation
        .as_ref()
        .map(|generation| {
            generation
                .manifest()
                .source_routes()
                .iter()
                .filter(|route| !route.sources().is_empty())
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    // `All` is a logical request over every route in this request's registry.
    // Express it to Core as an exact set so routes committed by an earlier
    // request-scoped explicit overlay are carried as read authority instead of
    // becoming automatic roots or accidental deletion decisions.
    let physical_scope = if scope == SourceBackedRefreshScope::All {
        let current_route_ids = build
            .registry
            .watch_catalog()
            .route_ids()
            .cloned()
            .collect::<BTreeSet<_>>();
        SourceBackedRefreshScope::exact(current_route_ids.difference(covered_route_ids).cloned())
    } else {
        scope.clone()
    };
    let expected_selected_route_ids = match &physical_scope {
        SourceBackedRefreshScope::Exact(routes) => routes
            .iter()
            .map(|route| route.as_str().to_owned())
            .chain(
                registry_failures
                    .iter()
                    .map(|failure| failure.route_identity.as_str().to_owned()),
            )
            .collect::<BTreeSet<_>>(),
        SourceBackedRefreshScope::All => {
            bail!("capture-owned physical refresh scope was not bounded to exact routes")
        }
    };
    if retained_generation.is_none()
        && !registry_failures.is_empty()
        && selected_registry_route_count(&build.registry, &physical_scope) == 0
    {
        return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
            failed_routes: SourceBackedSourceFailures::from_failures(
                registry_failures.iter().cloned(),
            ),
        }
        .into());
    }
    let previous_generation = retained_generation
        .as_ref()
        .map(|index| index.generation_id().to_owned());
    // Observation certificates are sampled before parsing. Terminal source
    // revalidation may legitimately accept same-file JSONL growth after the
    // scanned prefix, so sampling later could certify bytes absent from this
    // generation and make restart skip them. A pre-scan token is either bound
    // to the captured state or conservatively forces the next warm refresh.
    let admitted_route_observations = admitted_route_observations(&build.registry, &physical_scope);
    let writer_options = if build
        .registry
        .selected_routes_use_parallel_leaf_workers(&physical_scope)
    {
        source_backed_refresh_writer_options()
    } else {
        WriterOptions::default()
    };
    let (executor, _issues) = build.into_refresh_executor(writer_options);
    let executor = executor.with_base_route_controls(previous_route_controls.clone());
    let eta_execution_eligible = retained_generation.is_none()
        && scope == SourceBackedRefreshScope::All
        && covered_route_ids.is_empty()
        && registry_failures.is_empty();
    let mut report_attempt_progress = |mut update: CaptureSourceBackedDetailedRefreshProgress| {
        if !eta_execution_eligible {
            update.exact_scan_progress = None;
        }
        report_progress(update)
    };
    let mut terminal_coverage_error = None;
    let mut reconciliation_required = false;
    let refresh_result = executor
        .refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            physical_scope,
            reconciliation_demand,
            route_worksets.clone(),
            &mut report_attempt_progress,
            |context| {
                run_after_capture_scan_before_metadata_hook();
                let successful_route_outcomes = context.successful_route_outcomes();
                let failed_routes = context.failed_route_outcomes();
                let source_failures = context.source_failures();
                let complete_inventory_route_ids = context
                    .complete_inventory_route_ids()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if exact_catalog_members
                    && exact_member_family_fallback_required(
                        &route_worksets,
                        &complete_inventory_route_ids,
                        successful_route_outcomes,
                        &failed_routes,
                    )
                {
                    reconciliation_required = true;
                    return Err(IndexError::PublicationMetadata(
                        ExactMemberFallbackRequired.to_string(),
                    ));
                }
                let route_results = provider_route_results(
                    ProviderPublicationFacts {
                        selected_route_ids: &context
                            .selected_route_ids()
                            .cloned()
                            .collect::<Vec<_>>(),
                        successful_route_outcomes,
                        failed_routes: &failed_routes,
                        source_failures: &source_failures,
                        logical_source_failures: context.logical_source_failures(),
                        record_rejections: context.record_rejections(),
                        snapshot: context.snapshot(),
                    },
                    &registry_failures,
                    &expected_selected_route_ids,
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let current = SourceBackedRefreshCurrent::from_sources(
                    context.snapshot().sources(),
                    context.removed_source_count(),
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let (published_explicit_source_catalog, catalog_route_bindings) =
                    reconcile_published_catalog_witness(
                        context.snapshot(),
                        previous_explicit_source_catalog.as_ref(),
                        &previous_catalog_route_bindings,
                        requested_explicit_source_catalog.as_ref(),
                        &requested_catalog_route_bindings,
                        &route_results,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let mut publication = SourceBackedRefreshPublication {
                    generation_id: context.generation_id().to_owned(),
                    published_explicit_source_catalog,
                    unsupported_routes: route_results
                        .iter()
                        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
                        .count(),
                    certified_source_count: current.source_count,
                    certified_source_bytes: current.certified_source_bytes,
                    current,
                    route_results,
                    zero_source_authority: Vec::new(),
                    catalog_route_bindings,
                    timings: SourceBackedRefreshTimings::default(),
                    verified_index: None,
                };
                covered_publication.apply_receipt(&mut publication);
                publication.zero_source_authority = match classify_inventory_disposition(
                    &publication,
                    &complete_inventory_route_ids,
                    &previous_nonempty_routes,
                    &route_less_blockers,
                ) {
                    SourceBackedInventoryDisposition::AuthoritativeContent => Vec::new(),
                    SourceBackedInventoryDisposition::AuthoritativeEmpty(authority) => authority,
                    SourceBackedInventoryDisposition::UnsupportedOrUnavailable(error) => {
                        let detail = error.to_string();
                        terminal_coverage_error = Some(error);
                        return Err(IndexError::PublicationMetadata(detail));
                    }
                };
                let route_observations = successful_route_outcomes
                    .iter()
                    .filter(|outcome| outcome.logical_source_failure_total == 0)
                    .filter(|outcome| {
                        context
                            .snapshot()
                            .source_route(&outcome.route_identity)
                            .is_some_and(|route| !route.is_missing())
                    })
                    .filter_map(|outcome| {
                        admitted_route_observations
                            .get(&outcome.route_identity)
                            .cloned()
                            .map(|observation| (outcome.route_identity.clone(), observation))
                    })
                    .collect();
                encode_publication_metadata(
                    request_id,
                    operation,
                    &scope,
                    previous_generation.as_deref(),
                    &publication,
                    route_observations,
                    context.route_controls().clone(),
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))
            },
        );
    let mut receipt = match refresh_result {
        Ok(receipt) => receipt,
        Err(error) => {
            if reconciliation_required {
                return Err(ExactMemberFallbackRequired.into());
            }
            if let Some(error) = terminal_coverage_error {
                return Err(error.into());
            }
            return Err(error).context("run capture-owned source-backed refresh");
        }
    };
    let unsupported_routes = receipt.unsupported_routes.len();
    let (disposition, verified_index) = receipt.take_verified_publication().ok_or_else(|| {
        anyhow!("capture-owned metadata publication returned no exact verified generation")
    })?;
    let timings = SourceBackedRefreshTimings {
        discovery_us: nonzero_duration_micros(receipt.discovery_duration),
        scan_stage_us: nonzero_duration_micros(exclusive_scan_stage_duration(
            receipt.scan_stage_duration,
            receipt.commit_duration,
        )),
        commit_us: nonzero_duration_micros(receipt.commit_duration),
    };
    if disposition == CapturePublicationDisposition::Published {
        let verified_index = Arc::new(verified_index.into_inner().into_verified_index());
        let mut publication = publication_from_verified_metadata(
            request_id,
            operation,
            &scope,
            timings,
            verified_index,
        )?;
        covered_publication.apply_timings(&mut publication);
        publication.unsupported_routes = unsupported_routes;
        return Ok(publication);
    }
    let current =
        SourceBackedRefreshCurrent::from_sources(&receipt.sources, receipt.removals.len())?;
    if current.source_count != receipt.certified_source_count
        || current.certified_source_bytes != receipt.certified_source_bytes
        || current.indexed_documents != receipt.commit.indexed_documents
    {
        bail!(
            "capture-owned source refresh receipt does not match its retained generation cardinalities"
        );
    }
    let route_results = provider_route_results(
        ProviderPublicationFacts {
            selected_route_ids: &receipt.selected_route_ids,
            successful_route_outcomes: &receipt.successful_route_outcomes,
            failed_routes: &receipt.failed_routes,
            source_failures: &receipt.source_failures,
            logical_source_failures: &receipt.logical_source_failures,
            record_rejections: &receipt.record_rejections,
            snapshot: receipt.commit.snapshot(),
        },
        &registry_failures,
        &expected_selected_route_ids,
    )?;
    let (published_explicit_source_catalog, catalog_route_bindings) =
        reconcile_published_catalog_witness(
            receipt.commit.snapshot(),
            previous_explicit_source_catalog.as_ref(),
            &previous_catalog_route_bindings,
            requested_explicit_source_catalog.as_ref(),
            &requested_catalog_route_bindings,
            &route_results,
        )?;
    let generation_id = std::mem::take(&mut receipt.commit.generation_id);
    let mut publication = SourceBackedRefreshPublication {
        generation_id,
        published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        route_results,
        zero_source_authority: Vec::new(),
        catalog_route_bindings,
        timings,
        verified_index: Some(Arc::new(verified_index.into_inner().into_verified_index())),
    };
    covered_publication.apply(&mut publication);
    publication.zero_source_authority = match classify_inventory_disposition(
        &publication,
        &receipt
            .complete_inventory_route_ids
            .iter()
            .cloned()
            .collect(),
        &previous_nonempty_routes,
        &route_less_blockers,
    ) {
        SourceBackedInventoryDisposition::AuthoritativeContent => Vec::new(),
        SourceBackedInventoryDisposition::AuthoritativeEmpty(authority) => authority,
        SourceBackedInventoryDisposition::UnsupportedOrUnavailable(error) => {
            return Err(error.into())
        }
    };
    let verified_index = publication
        .verified_index
        .as_ref()
        .ok_or_else(|| anyhow!("reused Core refresh publication lost its exact verified pin"))?;
    let route_control_changed = receipt.route_controls != previous_route_controls;
    if route_control_changed
        || (publication.current.source_count == 0
            && !verify_generation_query_readiness(verified_index)
                .context("decode Core source-refresh publication authority")?
                .is_ready())
    {
        let route_observations = receipt
            .successful_route_outcomes
            .iter()
            .filter(|outcome| outcome.logical_source_failure_total == 0)
            .filter(|outcome| {
                receipt
                    .commit
                    .snapshot()
                    .source_route(&outcome.route_identity)
                    .is_some_and(|route| !route.is_missing())
            })
            .filter_map(|outcome| {
                admitted_route_observations
                    .get(&outcome.route_identity)
                    .cloned()
                    .map(|observation| (outcome.route_identity.clone(), observation))
            })
            .collect();
        let metadata = encode_publication_metadata(
            request_id,
            operation,
            &scope,
            previous_generation.as_deref(),
            &publication,
            route_observations,
            receipt.route_controls.clone(),
        )?;
        let writer = GenerationWriter::open(index_root, WriterOptions::default())?
            .into_writer()
            .map_err(committed_generation_recovery_error)?;
        let recertified = Arc::new(
            writer.republish_current_publication_metadata(&publication.generation_id, metadata)?,
        );
        validate_recertified_metadata(request_id, operation, &scope, &recertified)?;
        publication.verified_index = Some(recertified);
    }
    Ok(publication)
}

fn exact_member_family_fallback_required(
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    complete_inventory_routes: &BTreeSet<SourceRouteIdentity>,
    successful_routes: &[SourceBackedSuccessfulRouteOutcome],
    failed_routes: &[SourceBackedFailedRouteOutcome],
) -> bool {
    let exact_routes = route_worksets.keys().collect::<BTreeSet<_>>();
    complete_inventory_routes
        .iter()
        .any(|route| exact_routes.contains(route))
        || successful_routes.iter().any(|outcome| {
            exact_routes.contains(&outcome.route_identity)
                && outcome.logical_source_failure_total != 0
        })
        || failed_routes
            .iter()
            .any(|outcome| exact_routes.contains(&outcome.route_identity))
}

fn register_automatic_hermes_profile_rename_retirements(
    build: &mut ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    retained_generation: Option<&VerifiedIndex>,
    previous_catalog_route_bindings: &[ExplicitSourceCatalogRouteBinding],
    previous_route_controls: &BTreeMap<SourceRouteIdentity, Vec<u8>>,
) -> Result<()> {
    let Some(retained_generation) = retained_generation else {
        return Ok(());
    };
    let explicit_route_ids = previous_catalog_route_bindings
        .iter()
        .map(|binding| binding.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    let current_automatic_hermes = build
        .registry
        .routes()
        .filter(|route| {
            route.source.provider == CaptureProvider::Hermes
                && route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route.selector_authority == SourceBackedSelectorAuthority::DiscoveredWinner
        })
        .filter_map(|route| route.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let stale = previous_route_controls
        .iter()
        .filter(|(route, _)| {
            !current_automatic_hermes.contains(*route)
                && !explicit_route_ids.contains(route.as_str())
                && retained_generation.manifest().source_route(route).is_some()
        })
        .filter_map(|(route, control)| {
            ctx_history_capture::hermes_route_control_database_identity(control)
                .map(|database_identity| (route.clone(), database_identity))
        })
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return Ok(());
    }
    for replacement in current_automatic_hermes {
        build
            .registry
            .retire_controlled_routes_after_success(&replacement, stale.clone())?;
    }
    Ok(())
}

fn classify_inventory_disposition(
    publication: &SourceBackedRefreshPublication,
    complete_inventory_routes: &BTreeSet<SourceRouteIdentity>,
    previous_nonempty_routes: &BTreeSet<SourceRouteIdentity>,
    route_less_blockers: &RouteLessRegistryBlockers,
) -> SourceBackedInventoryDisposition {
    if route_less_blockers.total != 0 && publication.route_results.is_empty() {
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            route_less_blockers.publication_error(),
        );
    }
    if publication.current.source_count != 0 {
        return SourceBackedInventoryDisposition::AuthoritativeContent;
    }
    if route_less_blockers.total != 0 {
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            route_less_blockers.publication_error(),
        );
    }
    if publication.route_results.is_empty() {
        if complete_inventory_routes.is_empty() && previous_nonempty_routes.is_empty() {
            return SourceBackedInventoryDisposition::AuthoritativeEmpty(Vec::new());
        }
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            ZeroSourcePublicationBlocked::new(
                "zero-source publication has no terminal route authority for retained or discovered routes",
            ),
        );
    }
    let covered = publication
        .zero_source_authority
        .iter()
        .map(|authority| (authority.route_identity.clone(), authority.kind))
        .collect::<BTreeMap<_, _>>();
    let mut authority = Vec::with_capacity(publication.route_results.len());
    for result in &publication.route_results {
        if !result.outcome.is_success() {
            let source_detail = result
                .source_failures
                .first()
                .map(|failure| format!(": {}", failure.detail))
                .unwrap_or_default();
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(format!(
                    "zero-source publication route {} did not complete authoritatively{}",
                    result.route_identity, source_detail,
                )),
            );
        }
        let Ok(route_identity) = SourceRouteIdentity::from_sha256(result.route_identity.clone())
        else {
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(
                    "zero-source publication contains an invalid route identity",
                ),
            );
        };
        let kind = covered
            .get(&route_identity)
            .copied()
            .or_else(|| {
                previous_nonempty_routes
                    .contains(&route_identity)
                    .then_some(SourceBackedZeroSourceAuthorityKind::ConfirmedDeletion)
            })
            .or_else(|| {
                complete_inventory_routes
                    .contains(&route_identity)
                    .then_some(SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory)
            });
        let Some(kind) = kind else {
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(format!(
                    "zero-source publication route {} has neither a complete empty inventory nor confirmed deletion",
                    route_identity.as_str(),
                )),
            );
        };
        authority.push(SourceBackedZeroSourceAuthority {
            generation_id: publication.generation_id.clone(),
            route_identity,
            kind,
        });
    }
    authority.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    SourceBackedInventoryDisposition::AuthoritativeEmpty(authority)
}

#[doc(hidden)]
pub fn exclusive_scan_stage_duration(
    scan_stage_duration: StdDuration,
    commit_duration: StdDuration,
) -> StdDuration {
    // The capture receipt measures scan-stage wall time from before the
    // writer opens through commit, and also reports commit independently.
    // Keep the exported buckets disjoint without creating a telemetry layer.
    scan_stage_duration.saturating_sub(commit_duration)
}

fn encode_publication_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    previous_generation: Option<&str>,
    publication: &SourceBackedRefreshPublication,
    route_observations: BTreeMap<SourceRouteIdentity, String>,
    route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
) -> Result<Vec<u8>> {
    let terminal = SourceBackedRefreshReceipt::from_verified_publication(
        previous_generation.map(str::to_owned),
        publication.generation_id.clone(),
        publication,
    )?;
    SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: request_id.to_owned(),
        operation,
        refresh_scope: scope.clone(),
        receipt: terminal.to_json(),
        route_observations,
        route_controls,
    }
    .encode()
    .map_err(Into::into)
}

fn publication_from_verified_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    timings: SourceBackedRefreshTimings,
    verified_index: Arc<VerifiedIndex>,
) -> Result<SourceBackedRefreshPublication> {
    let metadata = SourceBackedPublicationMetadata::decode(&verified_index)?;
    if metadata.request_id != request_id
        || metadata.operation != operation
        || metadata.refresh_scope != *scope
    {
        bail!("published Core source-refresh metadata does not match its exact request");
    }
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), &verified_index)?;
    let unsupported_routes = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
        .count();
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.published_generation,
        published_explicit_source_catalog: receipt.published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.current.source_count,
        certified_source_bytes: receipt.current.certified_source_bytes,
        current: receipt.current,
        route_results: receipt.route_results,
        zero_source_authority: receipt.zero_source_authority,
        catalog_route_bindings: receipt.catalog_route_bindings,
        timings,
        verified_index: Some(verified_index),
    })
}

fn validate_recertified_metadata(
    request_id: &str,
    operation: RefreshOperation,
    scope: &SourceBackedRefreshScope,
    verified_index: &VerifiedIndex,
) -> Result<()> {
    let metadata = SourceBackedPublicationMetadata::decode(verified_index)?;
    if metadata.request_id != request_id
        || metadata.operation != operation
        || metadata.refresh_scope != *scope
        || !metadata.certifies_generation(verified_index)
    {
        bail!("recertified Core source-refresh metadata does not match its exact request");
    }
    Ok(())
}

struct ProviderPublicationFacts<'a, S: ImmutableCaptureSnapshot + ?Sized> {
    selected_route_ids: &'a [SourceRouteIdentity],
    successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    failed_routes: &'a [SourceBackedFailedRouteOutcome],
    source_failures: &'a SourceBackedSourceFailures,
    logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    record_rejections: &'a SourceBackedRecordRejections,
    snapshot: &'a S,
}

fn provider_route_results<S: ImmutableCaptureSnapshot + ?Sized>(
    facts: ProviderPublicationFacts<'_, S>,
    registry_failures: &[SourceBackedFailedRoute],
    expected_selected_route_ids: &BTreeSet<String>,
) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let selected_route_ids = facts
        .selected_route_ids
        .iter()
        .chain(
            registry_failures
                .iter()
                .map(|failure| &failure.route_identity),
        )
        .map(|identity| identity.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if selected_route_ids.len()
        != facts
            .selected_route_ids
            .len()
            .saturating_add(registry_failures.len())
        || &selected_route_ids != expected_selected_route_ids
    {
        bail!("capture-owned source refresh receipt omitted, duplicated, or added selected route outcomes");
    }
    let mut source_failures = facts.source_failures.clone();
    source_failures.extend(registry_failures.iter().cloned());
    let failed_route_outcomes = facts
        .failed_routes
        .iter()
        .map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        })
        .chain(registry_failures.iter().map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        }))
        .collect::<BTreeMap<_, _>>();
    if failed_route_outcomes.len()
        != facts
            .failed_routes
            .len()
            .saturating_add(registry_failures.len())
    {
        bail!("capture-owned source refresh receipt contains duplicate failed routes");
    }
    let successful_route_changes = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.route_identity.as_str().to_owned(),
                (outcome.changed, outcome.logical_source_failure_total),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let failed_routes = failed_route_outcomes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if successful_route_changes.len() != facts.successful_route_outcomes.len()
        || !successful_route_changes
            .keys()
            .all(|route| selected_route_ids.contains(route))
        || !successful_route_changes
            .keys()
            .all(|route| !failed_routes.contains(route))
        || successful_route_changes
            .len()
            .saturating_add(failed_routes.len())
            != selected_route_ids.len()
    {
        bail!("capture-owned source refresh receipt has an incomplete or overlapping terminal route-result partition");
    }
    let committed_rejections = committed_route_rejected_records(facts.snapshot)?;
    let successful_route_rejections = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.route_identity.as_str().to_owned(),
                committed_rejections
                    .get(&outcome.route_identity)
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut diagnostics_by_route = BTreeMap::<String, Vec<_>>::new();
    for failure in source_failures.failures() {
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: failure.source_selector.clone(),
                detail: failure.detail.clone(),
            });
    }
    for failure in facts.logical_source_failures.failures() {
        let source_identity = source_key_identity(&failure.source);
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: source_identity.clone(),
                provider: failure.source.provider().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: format!("logical-source:{source_identity}"),
                detail: failure.detail.clone(),
            });
    }
    let mut rejections_by_route = BTreeMap::<String, Vec<_>>::new();
    for rejection in facts.record_rejections.rejections() {
        let route_identity = rejection.route_identity.as_str().to_owned();
        rejections_by_route
            .entry(route_identity.clone())
            .or_default()
            .push(SourceBackedRefreshRecordRejection {
                route_identity,
                source_identity: source_key_identity(&rejection.source),
                provider: rejection.provider.as_str().to_owned(),
                source_selector: rejection.source_selector.clone(),
                line: rejection.line_number,
                payload_type: rejection
                    .payload_type
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_owned()),
                class: rejection.class.as_str().to_owned(),
                detail: rejection.detail.clone(),
            });
    }
    let route_results = selected_route_ids
        .iter()
        .map(|route_identity| {
            let mut result = successful_route_changes
                .get(route_identity)
                .copied()
                .map(|(changed, source_failure_total)| {
                    let mut result =
                        SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), changed);
                    result.source_failure_total = source_failure_total;
                    result
                })
                .or_else(|| {
                    failed_route_outcomes
                        .get(route_identity)
                        .map(|(class, carried)| {
                            SourceBackedRefreshRouteResult::failed(
                                route_identity.clone(),
                                class.clone(),
                                *carried,
                            )
                        })
                })
                .ok_or_else(|| anyhow!("selected route has no terminal outcome"))?;
            result.source_failures = diagnostics_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.rejected_record_total = successful_route_rejections
                .get(route_identity)
                .copied()
                .unwrap_or_default();
            result.rejection_diagnostics = rejections_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    if !diagnostics_by_route.is_empty() || !rejections_by_route.is_empty() {
        bail!("capture-owned source refresh diagnostics name an unselected route");
    }
    Ok(route_results)
}

fn source_key_identity(source: &ctx_history_core::SourceKey) -> String {
    source
        .identity()
        .digest()
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn committed_route_rejected_records(
    snapshot: &(impl ImmutableCaptureSnapshot + ?Sized),
) -> Result<HashMap<SourceRouteIdentity, u64>> {
    let certificates = snapshot
        .sources()
        .iter()
        .map(|source| (source.observation().source().identity().digest(), source))
        .collect::<HashMap<_, _>>();
    snapshot
        .source_routes()
        .map(|route| {
            let total = route.sources().iter().try_fold(0_u64, |total, source| {
                let certificate = certificates
                    .get(&source.identity().digest())
                    .filter(|candidate| {
                        candidate.observation().source().exact_descriptor_eq(source)
                    })
                    .ok_or_else(|| {
                        anyhow!(
                            "committed route {} names a source without an exact certificate",
                            route.route_identity().as_str()
                        )
                    })?;
                total
                    .checked_add(certificate.counts().rejected_records)
                    .ok_or_else(|| anyhow!("committed route rejected-record total overflow"))
            })?;
            Ok((route.route_identity().clone(), total))
        })
        .collect()
}

pub(super) fn build_merged_source_backed_registry(
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

fn build_merged_source_backed_registry_with_automatic_routes(
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
    // A request overlay is not the whole durable explicit catalog. Keep every
    // unmatched retained explicit owner out of automatic discovery so those
    // base routes remain carried rather than being re-scanned under a new
    // automatic identity. An exact automatic watcher admission is the one
    // exception: it may reclaim the same provider/format/path after a one-shot
    // explicit import. Deduplicate only exact provider/format/path keys;
    // relocation deliberately preserves lineage while changing the path.
    if let Some(catalog) = previous_explicit_source_catalog.as_ref() {
        catalog.prepare_retained_discovery_report_with_automatic_routes(
            explicit_source_catalog,
            &mut report,
            admitted_automatic_routes,
        )?;
    }
    if let Some(catalog) = explicit_source_catalog {
        catalog.prepare_discovery_report(data_root, &mut report)?;
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

#[cfg(test)]
mod catalog_refresh_admission_tests {
    use super::*;
    use std::fs;

    use ctx_history_capture::{
        provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, SourceBackedRoute,
        SourceBackedRouteDriver,
    };

    struct UnusedPublishedState;

    impl PublishedSourceBackedStatePort for UnusedPublishedState {
        fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
            unreachable!("catalog admission does not open published state")
        }
    }

    #[test]
    fn exhaustive_exact_route_reuses_catalog_without_claiming_member_work() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("cwd");
        let data_root = temp.path().join("data");
        let index_root = temp.path().join("index");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let database = home.join("history.db");
        fs::write(&database, b"sqlite").unwrap();
        let source = provider_source_for_path(CaptureProvider::OpenCode, database);
        let route = SourceBackedRoute::automatic(
            source.clone(),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap();
        let route_identity = route.metadata().route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let discovery = DiscoveryContext::new(
            &home,
            &cwd,
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        );
        let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
        let execution = SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "route-local-exhaustive",
            RefreshOperation::Refresh,
            None,
            SourceBackedRefreshScope::exact([route_identity]),
            BTreeSet::new(),
            SourceBackedRefreshCoveredPublication::default(),
            &discovery,
            &UnusedPublishedState,
            &progress,
        )
        .with_reconciliation_demand(SourceBackedReconciliationDemand::Exhaustive)
        .with_watch_catalog_opt(Some(registry.watch_catalog()));

        let admission = catalog_refresh_admission(&execution)
            .expect("exact registered route should avoid global discovery");
        assert!(!admission.exact_members);
        assert_eq!(admission.report.sources, vec![source]);
    }

    #[test]
    fn catalog_selector_distinguishes_exact_members_from_global_fallbacks() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("cwd");
        let data_root = temp.path().join("data");
        let index_root = temp.path().join("index");
        let root = home.join("claude-projects");
        let member = root.join("project/session.jsonl");
        fs::create_dir_all(member.parent().unwrap()).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(&member, b"{}\n").unwrap();
        let source = provider_source_for_path(CaptureProvider::Claude, root.clone());
        let route = SourceBackedRoute::automatic(
            source.clone(),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap();
        let route_identity = route.metadata().route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let discovery = DiscoveryContext::new(
            &home,
            &cwd,
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        );
        let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
        let execution = SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "exact-member",
            RefreshOperation::Refresh,
            None,
            SourceBackedRefreshScope::exact([route_identity.clone()]),
            BTreeSet::new(),
            SourceBackedRefreshCoveredPublication::default(),
            &discovery,
            &UnusedPublishedState,
            &progress,
        )
        .with_route_worksets(BTreeMap::from([(
            route_identity.clone(),
            SourceBackedRefreshWorkset::members([member.clone()]),
        )]))
        .with_watch_catalog_opt(Some(registry.watch_catalog()));

        let exact = catalog_refresh_admission(&execution)
            .expect("valid registered member should stay route-local");
        assert!(exact.exact_members);
        assert_eq!(exact.report.sources, vec![source]);

        let mut invalid_member = execution.clone();
        invalid_member.route_worksets = BTreeMap::from([(
            route_identity.clone(),
            SourceBackedRefreshWorkset::members([root.join("missing.jsonl")]),
        )]);
        let invalid_member = catalog_refresh_admission(&invalid_member)
            .expect("invalid member should retain route-local exhaustive work");
        assert!(!invalid_member.exact_members);

        let mut all = execution.clone();
        all.scope = SourceBackedRefreshScope::All;
        assert!(catalog_refresh_admission(&all).is_none());

        let mut unknown = execution.clone();
        unknown.scope =
            SourceBackedRefreshScope::exact([
                SourceRouteIdentity::from_sha256("ef".repeat(32)).unwrap()
            ]);
        assert!(catalog_refresh_admission(&unknown).is_none());

        fs::remove_dir_all(root).unwrap();
        assert!(catalog_refresh_admission(&execution).is_none());
    }

    #[test]
    fn complete_inventory_member_fallback_runs_only_once() {
        let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
        let complete = BTreeSet::from([route.clone()]);
        let members = BTreeMap::from([(route, BTreeSet::from([PathBuf::from("changed.json")]))]);

        assert!(exact_member_family_fallback_required(
            &members,
            &complete,
            &[],
            &[],
        ));
        assert!(!exact_member_family_fallback_required(
            &BTreeMap::new(),
            &complete,
            &[],
            &[],
        ));
    }
}
