use super::*;

mod exact_scan;
mod model;
mod ownership;
mod route_content;
mod route_outcomes;

use ctx_history_capture_model::{
    source_level_progress, AttemptHistoryProgress, SourceRecordProgress,
};
pub use ctx_history_capture_model::{
    SourceBackedDetailedRefreshProgress, SourceBackedRefreshProgress,
};
pub use ctx_history_capture_runtime::SourceBackedCertifiedRemoval;
use exact_scan::AttemptExactScanAccounting;
#[cfg(test)]
pub use model::assert_carried_route_failure;
pub use model::{
    SourceBackedPublicationMetadataContext, SourceBackedRefreshReceipt,
    SourceBackedSuccessfulRouteOutcome,
};
use model::{SourceBackedRefreshPlan, SourceBackedVerifiedPublication};
use route_content::source_route_content_fingerprints;
use route_outcomes::successful_route_outcomes_for_snapshot;

const SOURCE_RECORD_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

type SourceBackedPublicationMetadataFactory<'factory> =
    dyn for<'context> FnMut(
            SourceBackedPublicationMetadataContext<'context>,
        ) -> ctx_history_index::Result<Vec<u8>>
        + 'factory;

#[derive(Debug, Clone, Copy)]
struct SourceBackedRefreshExecutionBudget {
    discovery_duration: Duration,
    work_budget: usize,
}

impl SourceBackedRefreshExecutionBudget {
    const fn new(discovery_duration: Duration, work_budget: usize) -> Self {
        Self {
            discovery_duration,
            work_budget,
        }
    }
}

#[cfg(test)]
use ownership::source_owner_covers_base_source;
use ownership::{
    capture_staged_source_route_revalidation_receipts, require_complete_base_source_ownership,
    revalidate_staged_source_route,
};

#[cfg(test)]
thread_local! {
    static BEFORE_SOURCE_BACKED_COMMIT_HOOK: std::cell::RefCell<
        Option<Box<dyn FnOnce()>>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn install_before_source_backed_commit_hook_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "source-backed precommit test hooks must not be nested"
        );
    });
}

#[cfg(test)]
fn run_before_source_backed_commit_hook() {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_source_backed_commit_hook() {}

/// Capture-owned executor that can be installed behind the daemon's
/// provider-neutral `SourceBackedRefreshExecutor` callback seam.
#[derive(Debug, Clone)]
pub struct SourceBackedRefreshExecutor {
    registry: SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
    base_route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

impl SourceBackedRefreshExecutor {
    pub fn new(registry: SourceBackedProviderRegistry, writer_options: WriterOptions) -> Self {
        Self::with_discovery_duration(registry, writer_options, Duration::ZERO)
    }

    pub fn with_discovery_duration(
        registry: SourceBackedProviderRegistry,
        writer_options: WriterOptions,
        discovery_duration: Duration,
    ) -> Self {
        let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
        Self {
            registry,
            writer_options,
            discovery_duration,
            work_budget,
            base_route_controls: BTreeMap::new(),
        }
    }

    pub fn with_base_route_controls(
        mut self,
        controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
    ) -> Self {
        self.base_route_controls = controls;
        self
    }

    pub fn registry(&self) -> &SourceBackedProviderRegistry {
        &self.registry
    }

    pub fn refresh(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
                &self.base_route_controls,
            ),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    pub fn refresh_scope(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope),
                &self.base_route_controls,
            ),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_scope_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    pub fn refresh_scope_with_detailed_progress_and_reconciliation(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope)
                    .with_reconciliation_demand(reconciliation_demand),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    /// Publishes one scope with control-plane metadata bound into the same
    /// opaque Core commit payload. The factory runs only for a pointer-
    /// advancing generation; exact reuse retains the active metadata.
    pub fn refresh_scope_with_detailed_progress_and_publication_metadata(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        self.refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
            index_root,
            scope,
            SourceBackedReconciliationDemand::Exhaustive,
            report_progress,
            metadata_factory,
        )
    }

    pub fn refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        self.refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            scope,
            reconciliation_demand,
            BTreeMap::new(),
            report_progress,
            metadata_factory,
        )
    }

    pub fn refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        route_worksets: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        mut metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope)
                    .with_reconciliation_demand(reconciliation_demand)
                    .with_route_worksets(route_worksets),
                &self.base_route_controls,
            ),
            report_progress,
            Some(&mut metadata_factory),
        )
    }
}

/// Runs every executable route against one writer and publishes one atomic
/// generation. This is the capture-owned executor seam for the daemon.
pub fn refresh_source_backed_generation(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_progress(index_root, registry, writer_options, |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_work_budget_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    work_budget: usize,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_resource_limits_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    maximum_live_output_bytes: u64,
    maximum_physical_scratch_bytes: u64,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All)
                .with_resource_limits(maximum_live_output_bytes, maximum_physical_scratch_bytes),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

pub fn refresh_source_backed_generation_with_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let mut report_progress = report_progress;
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        move |update| {
            if update.current_source_progress.is_some() {
                return Ok(());
            }
            report_progress(update.into_legacy())
        },
        None,
    )
}

pub fn refresh_source_backed_generation_with_detailed_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        report_progress,
        None,
    )
}

pub fn refresh_source_backed_generation_for_routes(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    route_identities: impl IntoIterator<Item = SourceRouteIdentity>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::exact(route_identities)),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

fn refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    execution: SourceBackedRefreshExecutionBudget,
    selection: (
        SourceBackedRefreshPlan,
        &BTreeMap<SourceRouteIdentity, Vec<u8>>,
    ),
    mut emit_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    mut metadata_factory: Option<&mut SourceBackedPublicationMetadataFactory<'_>>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let (plan, base_route_controls) = selection;
    let SourceBackedRefreshExecutionBudget {
        discovery_duration,
        work_budget,
    } = execution;
    if matches!(&plan.scope, SourceBackedRefreshScope::All) {
        if let Some(unavailable) = registry.routes.iter().find(|route| {
            route.driver.is_none()
                && route.certified_missing_paths.is_empty()
                && route.metadata.source.status == ProviderSourceStatus::Unknown
        }) {
            return Err(SourceBackedCoordinatorError::UnavailableRoute {
                provider: unavailable.metadata.source.provider,
                detail: unavailable
                    .metadata
                    .unsupported_reason
                    .clone()
                    .unwrap_or_else(|| "route state is unavailable".to_owned()),
            });
        }
    }
    let executable_route_ids = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some() || !route.certified_missing_paths.is_empty())
        .filter_map(|route| route.metadata.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let selected_route_ids = match &plan.scope {
        SourceBackedRefreshScope::All => executable_route_ids,
        SourceBackedRefreshScope::Exact(selected) => {
            if let Some(unknown) = selected.difference(&executable_route_ids).next() {
                return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                    route_id: unknown.as_str().to_owned(),
                });
            }
            selected.clone()
        }
    };
    let scanned_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .filter(|route| {
            route
                .metadata
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected_route_ids.contains(identity))
        })
        .count();
    let mut selected_provider_set = HashSet::new();
    let providers = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .filter(|route| {
            route
                .metadata
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected_route_ids.contains(identity))
        })
        .filter_map(|route| {
            selected_provider_set
                .insert(route.metadata.source.provider)
                .then_some(route.metadata.source.provider)
        })
        .collect::<Vec<_>>();
    let attempt_history_progress = std::cell::RefCell::new(AttemptHistoryProgress::default());
    let exact_scan_accounting = std::cell::RefCell::new(AttemptExactScanAccounting::default());
    let exact_scan_accounting_valid = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut report_progress = |mut update: SourceBackedDetailedRefreshProgress| {
        update.exact_scan_progress = exact_scan_accounting_valid
            .load(std::sync::atomic::Ordering::SeqCst)
            .then(|| exact_scan_accounting.borrow().snapshot(scanned_routes))
            .flatten();
        emit_progress(update)
    };
    let refresh_started = Instant::now();
    report_progress(source_level_progress(SourceBackedRefreshProgress {
        phase: "discovering",
        completed_sources: 0,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        providers: providers.clone(),
        processed_sessions: 0,
        processed_messages: 0,
        processed_tool_calls: 0,
        processed_bytes: 0,
        stage_duration: discovery_duration,
        elapsed: discovery_duration,
        certified_source_count: None,
        certified_source_bytes: None,
    }))
    .map_err(SourceBackedCoordinatorError::Progress)?;
    let unsupported_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_none())
        .map(|route| route.metadata.clone())
        .collect();

    let scan_started = Instant::now();
    let index_root = index_root.as_ref();
    let mut failed_routes = BTreeMap::<SourceRouteIdentity, SourceBackedFailedRoute>::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut carried_unselected_route_ids = BTreeSet::new();

    let mut prepared_successful_route_outcomes = None;
    let (
        commit,
        applied_removals,
        complete_inventory_route_ids,
        commit_duration,
        base_route_content,
        mut route_controls,
        verified_publication,
    ) = {
        let open = IndexCaptureLifecycle::open(index_root, writer_options)?;
        let mut lifecycle = match open {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { recovery } => {
                let (generation_id, detail) = recovery.into_parts();
                return Err(
                    SourceBackedCoordinatorError::CommittedPredecessorMigrationRecovery {
                        generation_id,
                        detail,
                    },
                );
            }
        };
        let base_snapshot = lifecycle.base_snapshot();
        let base_route_content = source_route_content_fingerprints(base_snapshot.as_ref());
        let base_route_ids = lifecycle
            .base_snapshot()
            .map(|snapshot| {
                snapshot
                    .source_routes()
                    .map(|route| route.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        if matches!(&plan.scope, SourceBackedRefreshScope::Exact(_))
            && carried_unselected_route_ids.is_empty()
        {
            carried_unselected_route_ids = base_route_ids
                .difference(&selected_route_ids)
                .cloned()
                .collect();
        }
        if let Some(coordinator) = registry.codex_generation.as_ref() {
            let selected_participants = registry
                .routes
                .iter()
                .filter(|route| {
                    route
                        .metadata
                        .route_identity
                        .as_ref()
                        .is_some_and(|identity| selected_route_ids.contains(identity))
                })
                .filter_map(|route| route.codex_generation_participant)
                .collect::<Vec<_>>();
            if !selected_participants.is_empty() {
                coordinator
                    .select(&selected_participants)
                    .map_err(|error| SourceBackedCoordinatorError::RouteScan {
                        provider: CaptureProvider::Codex,
                        source: SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::InvalidSource,
                            error.to_string(),
                        ),
                    })?;
                let needs_exhaustive_catalog = registry.routes.iter().any(|route| {
                    route.codex_generation_participant.is_some()
                        && route
                            .metadata
                            .route_identity
                            .as_ref()
                            .is_some_and(|identity| {
                                selected_route_ids.contains(identity)
                                    && !plan.route_worksets.contains_key(identity)
                            })
                });
                if needs_exhaustive_catalog {
                    coordinator.prepare_selected().map_err(|error| {
                        SourceBackedCoordinatorError::RouteScan {
                            provider: CaptureProvider::Codex,
                            source: SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::InvalidSource,
                                error.to_string(),
                            ),
                        }
                    })?;
                }
            }
        }
        let attempt_selected = selected_route_ids.clone();
        let mut attempt_carried = carried_unselected_route_ids.clone();
        lifecycle.set_route_plan(attempt_selected.clone(), attempt_carried.clone())?;

        let automatic_missing_observed_at_unix_ms = source_missing_observation_time();
        let mut owners = HashMap::new();
        let mut complete_inventory_owners = Vec::new();
        let mut partial_routes = BTreeSet::new();
        let mut applied_removals = Vec::new();
        let mut successful_this_attempt = BTreeSet::new();
        let mut completed_routes = 0;
        for (route_index, route) in registry.routes.iter().enumerate() {
            let Some(route_identity) = route.metadata.route_identity.as_ref() else {
                continue;
            };
            if !attempt_selected.contains(route_identity) {
                continue;
            }
            let Some(driver) = &route.driver else {
                continue;
            };
            exact_scan_accounting.borrow_mut().begin_route();
            let history_progress = attempt_history_progress.borrow().snapshot();
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "refreshing",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: Some(route.metadata.source.path.display().to_string()),
                completed_records: Some(0),
                completed_bytes: Some(0),
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: scan_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(SourceBackedCoordinatorError::Progress)?;
            lifecycle.begin_route_stage(route_identity.clone())?;
            if let Some(revalidate) = driver.revalidate_at_publication.as_ref() {
                let revalidate = Arc::clone(revalidate);
                let accounting_valid = Arc::clone(&exact_scan_accounting_valid);
                lifecycle.register_route_revalidation(route_identity.clone(), move || {
                    let valid = revalidate();
                    if !valid {
                        accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    valid
                })?;
            }
            let removal_checkpoint = applied_removals.len();
            let logical_failure_checkpoint =
                logical_source_failures.checkpoint(route_identity.clone());
            let record_rejection_checkpoint = record_rejections.checkpoint();
            let current_source = route.metadata.source.path.display().to_string();
            let record_progress = std::cell::RefCell::new(SourceRecordProgress::default());
            let progress_failure = std::cell::RefCell::new(None::<SourceBackedRouteError>);
            for retired_route in &route.retire_after_success {
                lifecycle.authorize_carried_route_retirement(route_identity, retired_route)?;
            }
            let scan_result = {
                let progress_callback = std::cell::RefCell::new(&mut report_progress);
                let mut report_record_progress = |delta| {
                    if let Some(error) = progress_failure.borrow().as_ref() {
                        return Err(SourceBackedCoordinatorError::Progress(error.clone()));
                    }
                    exact_scan_accounting.borrow_mut().observe(&delta);
                    attempt_history_progress.borrow_mut().advance(&delta);
                    let Some(source_progress) = record_progress.borrow_mut().advanced_at(
                        delta,
                        Instant::now(),
                        SOURCE_RECORD_PROGRESS_INTERVAL,
                    ) else {
                        return Ok(());
                    };
                    let history_progress = attempt_history_progress.borrow().snapshot();
                    match progress_callback.borrow_mut()(source_level_progress(
                        SourceBackedRefreshProgress {
                            phase: "refreshing",
                            completed_sources: completed_routes,
                            total_sources: scanned_routes,
                            current_source: Some(current_source.clone()),
                            completed_records: Some(source_progress.completed_records),
                            completed_bytes: Some(source_progress.completed_bytes),
                            providers: providers.clone(),
                            processed_sessions: history_progress.processed_sessions,
                            processed_messages: history_progress.processed_messages,
                            processed_tool_calls: history_progress.processed_tool_calls,
                            processed_bytes: history_progress.processed_bytes,
                            stage_duration: scan_started.elapsed(),
                            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                            certified_source_count: None,
                            certified_source_bytes: None,
                        },
                    )) {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            progress_failure.replace(Some(error.clone()));
                            Err(SourceBackedCoordinatorError::Progress(error))
                        }
                    }
                };
                let mut report_current_source_progress = |current_source_progress| {
                    if let Some(error) = progress_failure.borrow().as_ref() {
                        return Err(error.clone());
                    }
                    let history_progress = attempt_history_progress.borrow().snapshot();
                    match progress_callback.borrow_mut()(SourceBackedDetailedRefreshProgress {
                        progress: SourceBackedRefreshProgress {
                            phase: "refreshing",
                            completed_sources: completed_routes,
                            total_sources: scanned_routes,
                            current_source: Some(current_source.clone()),
                            completed_records: None,
                            completed_bytes: None,
                            providers: providers.clone(),
                            processed_sessions: history_progress.processed_sessions,
                            processed_messages: history_progress.processed_messages,
                            processed_tool_calls: history_progress.processed_tool_calls,
                            processed_bytes: history_progress.processed_bytes,
                            stage_duration: scan_started.elapsed(),
                            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                            certified_source_count: None,
                            certified_source_bytes: None,
                        },
                        current_source_progress: Some(current_source_progress),
                        exact_scan_progress: None,
                    }) {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            progress_failure.replace(Some(error.clone()));
                            Err(error)
                        }
                    }
                };
                let core_record_preparer = lifecycle.core_preparation();
                let mut sink = SourceBackedGenerationSink {
                    lifecycle: &mut lifecycle,
                    core_record_preparer,
                    owners: &mut owners,
                    complete_inventories: &mut complete_inventory_owners,
                    applied_removals: &mut applied_removals,
                    route_index,
                    route_identity: route_identity.clone(),
                    base_route_control: base_route_controls.get(route_identity).cloned(),
                    resources: plan.route_resources_for(route_identity, work_budget),
                    logical_source_failures: &mut logical_source_failures,
                    record_rejections: &mut record_rejections,
                    record_progress: Some(&mut report_record_progress),
                    current_source_progress: Some(&mut report_current_source_progress),
                    last_progress_session_id: None,
                    exact_scan_total_bytes: None,
                    exact_scan_accounting_enabled: false,
                };
                (driver.scan)(&mut sink)
            };
            let mut record_progress = record_progress.into_inner();
            if let Some(error) = progress_failure.into_inner() {
                return Err(SourceBackedCoordinatorError::Progress(error));
            }
            if let Some(source_progress) = record_progress.flush_at(Instant::now()) {
                let history_progress = attempt_history_progress.borrow().snapshot();
                report_progress(source_level_progress(SourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: completed_routes,
                    total_sources: scanned_routes,
                    current_source: Some(current_source),
                    completed_records: Some(source_progress.completed_records),
                    completed_bytes: Some(source_progress.completed_bytes),
                    providers: providers.clone(),
                    processed_sessions: history_progress.processed_sessions,
                    processed_messages: history_progress.processed_messages,
                    processed_tool_calls: history_progress.processed_tool_calls,
                    processed_bytes: history_progress.processed_bytes,
                    stage_duration: scan_started.elapsed(),
                    elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                    certified_source_count: None,
                    certified_source_bytes: None,
                }))
                .map_err(SourceBackedCoordinatorError::Progress)?;
            }
            let terminal_route_for_eta = exact_scan_accounting
                .borrow_mut()
                .finish_route(scan_result.is_ok());
            match scan_result {
                Ok(()) => {
                    let replacement_control_identity =
                        if route.controlled_retire_after_success.is_empty() {
                            None
                        } else {
                            driver
                                .publication_control
                                .as_ref()
                                .map(|control| {
                                    control().map_err(|source| {
                                        SourceBackedCoordinatorError::RouteScan {
                                            provider: route.metadata.source.provider,
                                            source,
                                        }
                                    })
                                })
                                .transpose()?
                                .flatten()
                                .as_deref()
                                .and_then(|control| {
                                    driver.route_control_expectation.as_ref().and_then(
                                        |expectation| expectation.retirement_identity(control),
                                    )
                                })
                        };
                    let dynamic_retirements = route
                        .controlled_retire_after_success
                        .iter()
                        .filter(|candidate| {
                            Some(candidate.expected_identity) == replacement_control_identity
                        })
                        .map(|candidate| candidate.route_identity.clone())
                        .collect::<Vec<_>>();
                    for retired_route in &dynamic_retirements {
                        lifecycle
                            .authorize_carried_route_retirement(route_identity, retired_route)?;
                    }
                    let route_is_partial = lifecycle.route_retains_unstaged_members(route_identity);
                    capture_staged_source_route_revalidation_receipts(
                        &lifecycle,
                        route_index,
                        &mut owners,
                    )?;
                    let history_progress = attempt_history_progress.borrow().snapshot();
                    report_progress(source_level_progress(SourceBackedRefreshProgress {
                        phase: "verifying",
                        completed_sources: completed_routes,
                        total_sources: scanned_routes,
                        current_source: None,
                        completed_records: None,
                        completed_bytes: None,
                        providers: providers.clone(),
                        processed_sessions: history_progress.processed_sessions,
                        processed_messages: history_progress.processed_messages,
                        processed_tool_calls: history_progress.processed_tool_calls,
                        processed_bytes: history_progress.processed_bytes,
                        stage_duration: scan_started.elapsed(),
                        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                        certified_source_count: None,
                        certified_source_bytes: None,
                    }))
                    .map_err(SourceBackedCoordinatorError::Progress)?;
                    if revalidate_staged_source_route(
                        route.metadata.source.provider,
                        route_index,
                        driver,
                        &owners,
                        &complete_inventory_owners,
                    )? {
                        for retired_route in route
                            .retire_after_success
                            .iter()
                            .chain(&dynamic_retirements)
                        {
                            let retired_sources =
                                lifecycle.retire_carried_route(route_identity, retired_route)?;
                            attempt_carried.remove(retired_route);
                            carried_unselected_route_ids.remove(retired_route);
                            for source in retired_sources {
                                let digest = source.identity().digest();
                                match owners.entry(digest) {
                                    std::collections::hash_map::Entry::Vacant(entry) => {
                                        entry.insert(SourceOwner {
                                            route_index,
                                            source,
                                            present: false,
                                            revalidation: None,
                                        });
                                    }
                                    std::collections::hash_map::Entry::Occupied(entry)
                                        if entry.get().route_index == route_index
                                            && entry.get().source.exact_descriptor_eq(&source) => {}
                                    std::collections::hash_map::Entry::Occupied(_) => {
                                        return Err(
                                            SourceBackedCoordinatorError::DuplicateSourceOwner {
                                                source_id: source.identity().to_string(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        lifecycle.finish_route_stage(route_identity)?;
                        if route_is_partial {
                            partial_routes.insert(route_identity.clone());
                        }
                        successful_this_attempt.insert(route_identity.clone());
                    } else {
                        if !terminal_route_for_eta {
                            exact_scan_accounting.borrow_mut().revoke();
                            exact_scan_accounting_valid
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                        lifecycle.rollback_route_stage(route_identity)?;
                        owners.retain(|_, owner| owner.route_index != route_index);
                        complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                        applied_removals.truncate(removal_checkpoint);
                        logical_source_failures.truncate(logical_failure_checkpoint);
                        record_rejections
                            .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                        let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                        attempt_carried.insert(route_identity.clone());
                        failed_routes.insert(
                            route_identity.clone(),
                            source_backed_failed_route_from_route(
                                route,
                                SourceBackedSourceFailureClass::SourceChanged,
                                carried_forward,
                                "source route changed during terminal revalidation",
                            )?,
                        );
                    }
                }
                Err(source) => {
                    let Some(class) = source.kind.source_failure_class() else {
                        return Err(SourceBackedCoordinatorError::RouteScan {
                            provider: route.metadata.source.provider,
                            source,
                        });
                    };
                    if !terminal_route_for_eta {
                        exact_scan_accounting.borrow_mut().revoke();
                        exact_scan_accounting_valid
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    lifecycle.rollback_route_stage(route_identity)?;
                    owners.retain(|_, owner| owner.route_index != route_index);
                    complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                    applied_removals.truncate(removal_checkpoint);
                    logical_source_failures.truncate(logical_failure_checkpoint);
                    record_rejections
                        .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                    let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                    attempt_carried.insert(route_identity.clone());
                    failed_routes.insert(
                        route_identity.clone(),
                        source_backed_failed_route_from_route(
                            route,
                            class,
                            carried_forward,
                            &source.detail,
                        )?,
                    );
                }
            }
            completed_routes += 1;
        }

        for route in registry
            .routes
            .iter()
            .filter(|route| !route.certified_missing_paths.is_empty())
        {
            let route_identity = route.metadata.route_identity.as_ref().ok_or_else(|| {
                index_writer_invariant("certified-missing source route has no route identity")
            })?;
            if !attempt_selected.contains(route_identity) {
                continue;
            }
            lifecycle.begin_route_stage(route_identity.clone())?;
            let history_progress = attempt_history_progress.borrow().snapshot();
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "verifying",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: scan_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(SourceBackedCoordinatorError::Progress)?;
            let paths = route.certified_missing_paths.clone();
            if !paths
                .iter()
                .all(|path| path_presence(path) == PathPresence::Missing)
            {
                exact_scan_accounting.borrow_mut().revoke();
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                lifecycle.rollback_route_stage(route_identity)?;
                let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                attempt_carried.insert(route_identity.clone());
                failed_routes.insert(
                    route_identity.clone(),
                    source_backed_failed_route_from_route(
                        route,
                        SourceBackedSourceFailureClass::SourceChanged,
                        carried_forward,
                        "certified-missing route changed during terminal verification",
                    )?,
                );
                continue;
            }
            let accounting_valid = Arc::clone(&exact_scan_accounting_valid);
            lifecycle.observe_missing_route(
                route_identity.clone(),
                automatic_missing_observed_at_unix_ms,
                move || {
                    let valid = paths
                        .iter()
                        .all(|path| path_presence(path) == PathPresence::Missing);
                    if !valid {
                        accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    valid
                },
            )?;
            lifecycle.finish_route_stage(route_identity)?;
            successful_this_attempt.insert(route_identity.clone());
        }

        lifecycle.set_present_routes(registry.routes.iter().enumerate().filter_map(
            |(route_index, route)| {
                let route_identity = route.metadata.route_identity.as_ref()?;
                if route.driver.is_none()
                    || !successful_this_attempt.contains(route_identity)
                    || partial_routes.contains(route_identity)
                {
                    return None;
                }
                let members = owners
                    .values()
                    .filter(|owner| owner.route_index == route_index && owner.present)
                    .map(|owner| owner.source.clone())
                    .collect();
                Some(PresentCaptureRoute::new(route_identity.clone(), members))
            },
        ))?;

        require_complete_base_source_ownership(
            &lifecycle,
            registry,
            &owners,
            &complete_inventory_owners,
            &attempt_carried,
            &partial_routes,
        )?;

        let has_carried_source = lifecycle.base_snapshot().is_some_and(|base| {
            base.source_routes().any(|route| {
                attempt_carried.contains(route.route_identity()) && !route.sources().is_empty()
            })
        });
        let has_successful_retained_source = lifecycle.base_snapshot().is_some_and(|base| {
            base.source_routes().any(|route| {
                successful_this_attempt.contains(route.route_identity())
                    && !route.sources().is_empty()
            })
        });
        let has_successful_source = owners.values().any(|owner| owner.present);
        if (!failed_routes.is_empty() || !logical_source_failures.is_empty())
            && !has_carried_source
            && !has_successful_retained_source
            && !has_successful_source
        {
            if failed_routes.is_empty() {
                return Err(SourceBackedCoordinatorError::NoUsableLogicalSources {
                    failed_sources: logical_source_failures.clone(),
                });
            }
            return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: bounded_source_failures(failed_routes.values()),
            });
        }

        let history_progress = attempt_history_progress.borrow().snapshot();
        report_progress(source_level_progress(SourceBackedRefreshProgress {
            phase: "committing",
            completed_sources: scanned_routes,
            total_sources: scanned_routes,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            providers: providers.clone(),
            processed_sessions: history_progress.processed_sessions,
            processed_messages: history_progress.processed_messages,
            processed_tool_calls: history_progress.processed_tool_calls,
            processed_bytes: history_progress.processed_bytes,
            stage_duration: scan_started.elapsed(),
            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
            certified_source_count: None,
            certified_source_bytes: None,
        }))
        .map_err(SourceBackedCoordinatorError::Progress)?;
        let commit_started = Instant::now();
        run_before_source_backed_commit_hook();
        let mut revalidate_source = |target: CaptureRevalidationTarget<'_>| {
            let source = match target {
                CaptureRevalidationTarget::Source(source) => source.observation().source(),
                CaptureRevalidationTarget::Deletion(deletion) => deletion.source(),
            };
            let valid = owners
                .get(&source.identity().digest())
                .filter(|owner| owner.source.exact_descriptor_eq(source))
                .is_some_and(|owner| {
                    matches!(
                        (&owner.revalidation, target),
                        (
                            Some(SourceBackedRouteRevalidation::Source(expected)),
                            CaptureRevalidationTarget::Source(actual)
                        ) if *expected == *actual
                    ) || matches!(
                        (&owner.revalidation, target),
                        (
                            Some(SourceBackedRouteRevalidation::Deletion(expected)),
                            CaptureRevalidationTarget::Deletion(actual)
                        ) if *expected == *actual
                    )
                });
            if !valid {
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            valid
        };
        let mut revalidate_inventory = |inventory: &CertifiedSourceInventory| {
            let valid = complete_inventory_owners
                .iter()
                .any(|owner| owner.inventory == *inventory);
            if !valid {
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            valid
        };
        let complete_inventory_route_ids = complete_inventory_owners
            .iter()
            .filter_map(|owner| {
                registry
                    .routes
                    .get(owner.route_index)
                    .and_then(|route| route.metadata.route_identity.clone())
            })
            .chain(
                registry
                    .routes
                    .iter()
                    .filter(|route| !route.certified_missing_paths.is_empty())
                    .filter_map(|route| route.metadata.route_identity.as_ref())
                    .filter(|route_identity| successful_this_attempt.contains(*route_identity))
                    .cloned(),
            )
            .collect::<BTreeSet<_>>();
        let mut report_publication_stage = |stage: PublicationStage| {
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: stage.as_str(),
                completed_sources: scanned_routes,
                total_sources: scanned_routes,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: commit_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(|error| {
                IndexError::PublicationMetadata(format!(
                    "persist pre-publication progress: {error}"
                ))
            })
        };
        let mut route_controls = base_route_controls.clone();
        for route in &registry.routes {
            let Some(route_identity) = route.metadata.route_identity.as_ref() else {
                continue;
            };
            if !successful_this_attempt.contains(route_identity) {
                continue;
            }
            route_controls.remove(route_identity);
            let Some(control) = route
                .driver
                .as_ref()
                .and_then(|driver| driver.publication_control.as_ref())
            else {
                continue;
            };
            let Some(control) =
                control().map_err(|source| SourceBackedCoordinatorError::RouteScan {
                    provider: route.metadata.source.provider,
                    source,
                })?
            else {
                continue;
            };
            if control.len() > MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES {
                return Err(SourceBackedCoordinatorError::RouteScan {
                    provider: route.metadata.source.provider,
                    source: SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "route publication control exceeds its bounded contract",
                    ),
                });
            }
            route_controls.insert(route_identity.clone(), control);
        }
        let (commit, verified_publication) = if let Some(factory) = metadata_factory.as_mut() {
            let published = lifecycle.commit_with_metadata_and_progress(
                &mut revalidate_source,
                &mut revalidate_inventory,
                |publication| {
                    let mut live_route_controls = route_controls.clone();
                    live_route_controls
                        .retain(|route, _| publication.snapshot().source_route(route).is_some());
                    let outcomes = successful_route_outcomes_for_snapshot(
                        &selected_route_ids,
                        &failed_routes,
                        &logical_source_failures,
                        &base_route_content,
                        publication.snapshot(),
                    );
                    prepared_successful_route_outcomes = Some(outcomes.clone());
                    factory(SourceBackedPublicationMetadataContext::new(
                        publication,
                        &selected_route_ids,
                        &failed_routes,
                        &logical_source_failures,
                        &record_rejections,
                        &outcomes,
                        &complete_inventory_route_ids,
                        &live_route_controls,
                        applied_removals.len(),
                    ))
                },
                &mut report_publication_stage,
            )?;
            let (commit, disposition, verified) = published.into_parts();
            (
                IndexCaptureCommitReceipt::new(commit),
                Some(SourceBackedVerifiedPublication {
                    disposition,
                    verified_index: verified,
                }),
            )
        } else {
            (
                IndexCaptureCommitReceipt::new(
                    lifecycle.commit(&mut revalidate_source, &mut revalidate_inventory)?,
                ),
                None,
            )
        };
        (
            commit,
            applied_removals,
            complete_inventory_route_ids,
            commit_started.elapsed(),
            base_route_content,
            route_controls,
            verified_publication,
        )
    };

    let successful_route_ids = selected_route_ids
        .iter()
        .filter(|identity| !failed_routes.contains_key(*identity))
        .cloned()
        .collect::<BTreeSet<_>>();
    let successful_route_outcomes = prepared_successful_route_outcomes.unwrap_or_else(|| {
        successful_route_outcomes_for_snapshot(
            &selected_route_ids,
            &failed_routes,
            &logical_source_failures,
            &base_route_content,
            commit.snapshot(),
        )
    });
    for route in &registry.routes {
        if route
            .metadata
            .route_identity
            .as_ref()
            .is_some_and(|identity| successful_route_ids.contains(identity))
        {
            if let Some(after_publication) = route
                .driver
                .as_ref()
                .and_then(|driver| driver.after_successful_publication.as_ref())
            {
                after_publication();
            }
        }
    }
    let scan_stage_duration = scan_started.elapsed();
    let history_progress = attempt_history_progress.borrow().snapshot();
    let _ = report_progress(source_level_progress(SourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: scanned_routes,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        providers,
        processed_sessions: history_progress.processed_sessions,
        processed_messages: history_progress.processed_messages,
        processed_tool_calls: history_progress.processed_tool_calls,
        processed_bytes: history_progress.processed_bytes,
        stage_duration: commit_duration,
        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
        certified_source_count: Some(commit.certified_sources),
        certified_source_bytes: Some(commit.certified_source_bytes),
    }));
    let certified_source_count = commit.certified_sources;
    let certified_source_bytes = commit.certified_source_bytes;
    let sources = commit.snapshot().sources().to_vec();
    let source_failures = bounded_source_failures(failed_routes.values());
    route_controls.retain(|route, _| commit.snapshot().source_route(route).is_some());
    Ok(SourceBackedRefreshReceipt {
        commit,
        sources,
        removals: applied_removals,
        scanned_routes,
        unsupported_routes,
        discovery_duration,
        scan_stage_duration,
        commit_duration,
        certified_source_count,
        certified_source_bytes,
        selected_route_ids: selected_route_ids.into_iter().collect(),
        successful_route_ids: successful_route_ids.into_iter().collect(),
        successful_route_outcomes,
        complete_inventory_route_ids: complete_inventory_route_ids.into_iter().collect(),
        carried_unselected_route_ids: carried_unselected_route_ids.into_iter().collect(),
        carried_failed_route_ids: failed_routes
            .values()
            .filter(|failure| failure.carried_forward)
            .map(|failure| failure.route_identity.clone())
            .collect(),
        route_controls,
        source_failures,
        logical_source_failures,
        record_rejections,
        verified_publication,
        failed_routes: failed_routes
            .values()
            .map(SourceBackedFailedRouteOutcome::from)
            .collect(),
    })
}

fn bounded_source_failures<'a>(
    failures: impl IntoIterator<Item = &'a SourceBackedFailedRoute>,
) -> SourceBackedSourceFailures {
    SourceBackedSourceFailures::from_failures(failures.into_iter().cloned())
}

fn source_missing_observation_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod ownership_tests;
