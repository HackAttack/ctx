use super::*;

pub(super) struct MergedSourceBackedRegistry {
    pub(super) build: ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    previous_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    previous_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    retained_generation: Option<VerifiedIndex>,
    requested_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
}

enum SourceBackedInventoryDisposition {
    AuthoritativeContent,
    AuthoritativeEmpty(Vec<SourceBackedZeroSourceAuthority>),
    UnsupportedOrUnavailable(ZeroSourcePublicationBlocked),
}

pub(super) fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery_context = execution.discovery_context;
    execute_capture_owned_refresh_with(
        execution,
        discovery_context,
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
            refresh_all_provider_sources_route_local(
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
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
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
    execution.report_detailed_progress_with_total_state(
        "discovering",
        0,
        0,
        false,
        None,
        None,
        None,
        None,
    )?;
    let mut report_progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        let progress = update.progress;
        execution
            .report_detailed_progress(
                progress.phase,
                progress.completed_sources,
                progress.total_sources,
                progress.current_source,
                progress.completed_records,
                progress.completed_bytes,
                update
                    .current_source_progress
                    .map(SourceBackedCurrentSourceProgress::from_capture),
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
        execution.published_state,
        &mut report_progress,
    )
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
    let MergedSourceBackedRegistry {
        build,
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog,
        retained_generation,
        requested_catalog_route_bindings,
    } = build_merged_source_backed_registry(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        published_state,
    )?;
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
    let mut terminal_coverage_error = None;
    let refresh_result = executor.refresh_scope_with_detailed_progress_and_publication_metadata(
        index_root,
        physical_scope,
        report_progress,
        |context| {
            run_after_capture_scan_before_metadata_hook();
            let successful_route_outcomes = context.successful_route_outcomes();
            let failed_routes = context.failed_route_outcomes();
            let source_failures = context.source_failures();
            let route_results = provider_route_results(
                ProviderPublicationFacts {
                    selected_route_ids: &context.selected_route_ids().cloned().collect::<Vec<_>>(),
                    successful_route_outcomes,
                    failed_routes: &failed_routes,
                    source_failures: &source_failures,
                    logical_source_failures: context.logical_source_failures(),
                    record_rejections: context.record_rejections(),
                    manifest: context.manifest(),
                },
                &registry_failures,
                &expected_selected_route_ids,
            )
            .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
            let current = SourceBackedRefreshCurrent::from_sources(
                &context.manifest().sources,
                context.removed_source_count(),
            )
            .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
            let (published_explicit_source_catalog, catalog_route_bindings) =
                reconcile_published_catalog_witness(
                    previous_explicit_source_catalog.as_ref(),
                    &previous_catalog_route_bindings,
                    requested_explicit_source_catalog.as_ref(),
                    &requested_catalog_route_bindings,
                    context.manifest(),
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
                &context.complete_inventory_route_ids().cloned().collect(),
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
                        .manifest()
                        .source_route(&outcome.route_identity)
                        .is_some_and(|route| route.missing_state().is_none())
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
            )
            .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))
        },
    );
    let mut receipt = match refresh_result {
        Ok(receipt) => receipt,
        Err(error) => {
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
    if disposition == PublicationDisposition::Published {
        let verified_index = Arc::new(verified_index);
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
            manifest: receipt.commit.manifest(),
        },
        &registry_failures,
        &expected_selected_route_ids,
    )?;
    let (published_explicit_source_catalog, catalog_route_bindings) =
        reconcile_published_catalog_witness(
            previous_explicit_source_catalog.as_ref(),
            &previous_catalog_route_bindings,
            requested_explicit_source_catalog.as_ref(),
            &requested_catalog_route_bindings,
            receipt.commit.manifest(),
            &route_results,
        )?;
    let mut publication = SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id.clone(),
        published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        route_results,
        zero_source_authority: Vec::new(),
        catalog_route_bindings,
        timings,
        verified_index: Some(Arc::new(verified_index)),
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
    if publication.current.source_count == 0 && !verified_generation_is_query_ready(verified_index)?
    {
        let route_observations = receipt
            .successful_route_outcomes
            .iter()
            .filter(|outcome| outcome.logical_source_failure_total == 0)
            .filter(|outcome| {
                receipt
                    .commit
                    .manifest()
                    .source_route(&outcome.route_identity)
                    .is_some_and(|route| route.missing_state().is_none())
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
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            ZeroSourcePublicationBlocked::new(
                "zero-source publication has no executable certified route",
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

pub(crate) fn verified_generation_is_query_ready(index: &VerifiedIndex) -> Result<bool> {
    let Some(_) = index.publication_metadata() else {
        return Ok(!index.manifest().sources.is_empty());
    };
    let metadata = SourceBackedPublicationMetadata::decode(index)
        .context("decode Core source-refresh publication authority")?;
    Ok(metadata.certifies_generation(index))
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

struct ProviderPublicationFacts<'a> {
    selected_route_ids: &'a [SourceRouteIdentity],
    successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    failed_routes: &'a [SourceBackedFailedRouteOutcome],
    source_failures: &'a SourceBackedSourceFailures,
    logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    record_rejections: &'a SourceBackedRecordRejections,
    manifest: &'a GenerationManifest,
}

fn provider_route_results(
    facts: ProviderPublicationFacts<'_>,
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
    let committed_rejections = committed_route_rejected_records(facts.manifest)?;
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
    manifest: &GenerationManifest,
) -> Result<HashMap<SourceRouteIdentity, u64>> {
    let certificates = manifest
        .sources
        .iter()
        .map(|source| (source.observation().source().identity().digest(), source))
        .collect::<HashMap<_, _>>();
    manifest
        .source_routes()
        .iter()
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
    mut report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<MergedSourceBackedRegistry> {
    let PublishedSourceBackedState {
        verified_index: retained_generation,
        explicit_source_catalog: previous_explicit_source_catalog,
        catalog_route_bindings: previous_catalog_route_bindings,
    } = published_state.open_published_state(data_root)?;
    // A request overlay is not the whole durable explicit catalog. Keep every
    // unmatched retained explicit owner out of automatic discovery so those
    // base routes remain carried rather than being re-scanned under a new
    // automatic identity. Deduplicate only exact provider/format/path keys:
    // relocation deliberately preserves lineage while changing the path.
    if let Some(catalog) = previous_explicit_source_catalog.as_ref() {
        catalog.prepare_retained_discovery_report(explicit_source_catalog, &mut report)?;
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
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog: explicit_source_catalog.cloned(),
        retained_generation,
        requested_catalog_route_bindings,
    })
}
