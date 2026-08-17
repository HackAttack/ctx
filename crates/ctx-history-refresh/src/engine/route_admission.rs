use super::*;

impl CoreRefreshEngine {
    pub fn enqueue_next_scheduled_refresh(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, true)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_next_dirty_route(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, false)
    }

    fn enqueue_next_dirty_route_with_cold_all(
        &self,
        data_root: &Path,
        now_ms: u64,
        cold_all: bool,
    ) -> Result<bool> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let request_id = {
            let mut state = self.lock_state();
            if durable_queue_entry_count(&state) != 0 {
                return Ok(false);
            }
            let routes = state
                .dirty_routes
                .due_routes(now_ms, SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
            if routes.is_empty() {
                return Ok(false);
            }
            let requires_exhaustive_recovery = routes.iter().any(|route| {
                state
                    .hermes_routes_requiring_exhaustive_recovery
                    .contains(route)
                    || state
                        .routes_requiring_exhaustive_reconciliation
                        .contains(route)
            });
            let refresh_scope = if cold_all && observed_generation.is_none() {
                // A cold generation has no retained routes to carry. Publish
                // the complete startup inventory atomically instead of one
                // transient partial generation per initially dirty route.
                SourceBackedRefreshScope::All
            } else {
                SourceBackedRefreshScope::Exact(routes)
            };
            let mut attempt = new_refresh_attempt(
                observed_generation,
                SourceRefreshRuntimeMetadata::periodic(),
                None,
                refresh_scope,
            );
            if requires_exhaustive_recovery {
                attempt.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive;
            }
            let request_id = attempt.request_id.clone();
            state.active_request_id = Some(request_id.clone());
            state.attempts.push_back(attempt);
            trim_terminal_attempt_history(&mut state);
            request_id
        };
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    pub(super) fn background_maintenance_wake_response(
        &self,
        data_root: &Path,
        request_id: String,
    ) -> Result<Value> {
        let published_generation = self.observed_published_generation(data_root)?;
        let metadata = self
            .runtime
            .metadata(data_root, SourceBackedRefreshOperation::Refresh);
        Ok(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": request_id,
            "logical_request_id": request_id,
            "request_state": "queued",
            "logical_phase": "waiting",
            "previous_generation": published_generation.clone(),
            "published_generation": published_generation,
            "progress": {
                "phase": "maintenance_wake",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": false,
            },
            "daemon_mode": metadata.daemon_mode.as_str(),
            "trigger": metadata.trigger,
            "trigger_provenance": metadata.trigger_provenance,
            "maintenance_wake": true,
        })))
    }

    pub(super) fn finish_route_admissions(
        &self,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let mut state = self.lock_state();
        Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        )
    }

    pub(super) fn finish_route_admissions_and_persist(
        &self,
        data_root: &Path,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> Result<RouteAdmissionFinish> {
        let mut state = self.lock_state();
        let finish = Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        );
        let job = durable_job_json(&state, &finish.durable_request_id).ok_or_else(|| {
            anyhow!(
                "source refresh request `{}` disappeared during route finalization",
                finish.durable_request_id
            )
        })?;
        if let Err(error) = self.write_status(data_root, &job) {
            if finish.durable_request_id != request_id {
                state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                    request_id: finish.durable_request_id.clone(),
                    terminal_job: job,
                    outcome: PendingTerminalOutcome::Failed {
                        scheduler_retry: false,
                    },
                });
            }
            return Err(error);
        }
        Ok(finish)
    }

    fn finish_route_admissions_locked(
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let now_ms = source_route_ledger_now_ms();
        let admissions = state
            .route_admissions
            .remove(request_id)
            .unwrap_or_default();
        let retained_predecessor_event_watermarks =
            state.route_admission_watermarks.remove(request_id);
        if let Some(predecessor_event_watermarks) = retained_predecessor_event_watermarks.as_ref() {
            for continuation in state.manual_all_continuations.values_mut() {
                if continuation.predecessor_request_id == request_id {
                    continuation.predecessor_event_watermarks =
                        predecessor_event_watermarks.clone();
                }
            }
        }
        let predecessor_event_watermarks =
            retained_predecessor_event_watermarks.unwrap_or_default();
        let current_event_watermarks = state.route_event_watermarks.clone();
        let attempt = find_attempt(state, request_id).cloned();
        let route_results = attempt
            .as_ref()
            .and_then(|attempt| attempt.receipt.as_ref())
            .map(|receipt| {
                receipt
                    .route_results
                    .iter()
                    .map(|result| (result.route_identity.as_str(), result))
                    .collect::<BTreeMap<_, _>>()
            });
        let mut covered_route_results = BTreeMap::new();
        let mut certified_routes = BTreeMap::new();
        for admission in admissions {
            let terminal_failed = !publication_ready
                || attempt
                    .as_ref()
                    .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::Published);
            if terminal_failed {
                let blocked = attempt
                    .as_ref()
                    .and_then(|attempt| attempt.failure_outcome.as_ref())
                    .is_some_and(|outcome| outcome.blocked_routes.contains(admission.route()));
                if blocked {
                    state.dirty_routes.permanent_failure(&admission);
                } else {
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                    state
                        .routes_requiring_exhaustive_reconciliation
                        .insert(admission.route().clone());
                }
                continue;
            }
            let Some(result) = route_results
                .as_ref()
                .and_then(|results| results.get(admission.route().as_str()))
                .copied()
            else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                state
                    .routes_requiring_exhaustive_reconciliation
                    .insert(admission.route().clone());
                continue;
            };
            if let Some(retryable) = source_backed_route_retry_disposition(result) {
                if retryable {
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                    state
                        .routes_requiring_exhaustive_reconciliation
                        .insert(admission.route().clone());
                } else {
                    state.dirty_routes.permanent_failure(&admission);
                }
                continue;
            }
            if result.outcome.is_success() {
                let verified_boundary = attempt.as_ref().and_then(|attempt| {
                    let observation = attempt.route_observations.get(admission.route())?;
                    let admitted_watermark = predecessor_event_watermarks
                        .get(admission.route())
                        .copied()?;
                    let published_generation = attempt.published_generation.as_deref()?;
                    let covered_through =
                        post_publication_fence.map_or(admitted_watermark, |fence| {
                            fence.certified_boundary(
                                admission.route(),
                                admitted_watermark,
                                observation,
                            )
                        });
                    VerifiedSourceRefreshRouteBoundary::new(
                        request_id,
                        published_generation,
                        admission.route(),
                        covered_through,
                        observation,
                    )
                    .map(|boundary| (boundary, observation.clone()))
                });
                let acknowledged = match verified_boundary.as_ref() {
                    Some((boundary, _)) => state
                        .dirty_routes
                        .acknowledge_generation_coverage(&admission, boundary),
                    None => state.dirty_routes.acknowledge(&admission),
                };
                if acknowledged {
                    if attempt.as_ref().is_some_and(|attempt| {
                        attempt.reconciliation_demand
                            == SourceBackedReconciliationDemand::Exhaustive
                    }) {
                        state
                            .hermes_routes_requiring_exhaustive_recovery
                            .remove(admission.route());
                        state
                            .routes_requiring_exhaustive_reconciliation
                            .remove(admission.route());
                    }
                    covered_route_results.insert(admission.route().clone(), result.clone());
                    if let Some((boundary, observation)) = verified_boundary {
                        certified_routes.insert(
                            admission.route().clone(),
                            SourceBackedRefreshRouteCoverageCertificate {
                                observation,
                                admitted_watermark: boundary.covered_through(),
                            },
                        );
                    }
                }
            } else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                state
                    .routes_requiring_exhaustive_reconciliation
                    .insert(admission.route().clone());
            }
        }
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == SourceBackedRefreshState::Failed)
        {
            let durable_request_id = Self::terminalize_failed_predecessor_demands(
                state,
                request_id,
                attempt.as_ref().expect("failed predecessor snapshot"),
            )
            .unwrap_or_else(|| request_id.to_owned());
            if attempt
                .as_ref()
                .and_then(|attempt| attempt.failure_outcome.as_ref())
                .is_some_and(|outcome| !outcome.affected_routes.is_empty())
                && state.pending_scheduler_retry_root_id.as_deref() == Some(request_id)
            {
                state.pending_scheduler_retry_root_id = None;
            }
            return RouteAdmissionFinish {
                coverage_certificate: None,
                durable_request_id,
            };
        }
        let predecessor_reconciliation_demand = attempt
            .as_ref()
            .map(|attempt| attempt.reconciliation_demand)
            .unwrap_or(SourceBackedReconciliationDemand::Incremental);
        let successor_reconciliation_demands = state
            .attempts
            .iter()
            .map(|attempt| (attempt.request_id.clone(), attempt.reconciliation_demand))
            .collect::<BTreeMap<_, _>>();
        for (continuation_id, continuation) in &mut state.manual_all_continuations {
            if continuation.predecessor_request_id != request_id {
                continue;
            }
            continuation.predecessor_finished = true;
            if successor_reconciliation_demands
                .get(continuation_id)
                .is_some_and(|requested| predecessor_reconciliation_demand < *requested)
            {
                continue;
            }
            if let Some(attempt) = attempt.as_ref() {
                if let Some(receipt) = attempt.receipt.as_ref() {
                    for (route, admission_observation) in &continuation.admission_route_observations
                    {
                        let covered = !continuation.invalidated_routes.contains(route)
                            && continuation.admission_event_watermarks.get(route)
                                == continuation.predecessor_event_watermarks.get(route)
                            && admission_observation.as_ref().is_some_and(|admitted| {
                                attempt.route_observations.get(route) == Some(admitted)
                                    && receipt.route_results.iter().any(|result| {
                                        result.route_identity == route.as_str()
                                            && source_backed_route_retry_disposition(result)
                                                .is_none()
                                    })
                            });
                        if covered {
                            if let Some(result) = receipt
                                .route_results
                                .iter()
                                .find(|result| result.route_identity == route.as_str())
                            {
                                continuation
                                    .covered_route_results
                                    .insert(route.clone(), result.clone());
                                if let Some(authority) = receipt
                                    .zero_source_authority
                                    .iter()
                                    .find(|authority| authority.route_identity == *route)
                                {
                                    continuation
                                        .covered_zero_source_authority
                                        .insert(route.clone(), authority.clone());
                                }
                            }
                        }
                    }
                }
            }
            if covered_route_results.is_empty() {
                if continuation.covered_route_results.is_empty() {
                    continue;
                }
            } else {
                for (route, result) in &covered_route_results {
                    if continuation.invalidated_routes.contains(route) {
                        continue;
                    }
                    if attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.route_observations.contains_key(route))
                    {
                        // A predecessor with a provider-certified observation
                        // cannot be covered by a later indeterminate sample.
                        // The ledger path is only for route kinds that were
                        // indeterminate throughout the same successful pass.
                        continue;
                    }
                    if continuation
                        .admission_route_observations
                        .contains_key(route)
                        && !continuation.ledger_eligible_routes.contains(route)
                    {
                        continue;
                    }
                    // Legacy watcher-ledger admissions are a second exact
                    // coverage proof for routes outside the catalog-derived
                    // fence. Keep them in the durable logical demand, but do
                    // not let them override an indeterminate or mismatched
                    // catalog observation for the same route.
                    continuation
                        .admission_route_observations
                        .insert(route.clone(), None);
                    if let Some(watermark) = current_event_watermarks.get(route).copied() {
                        continuation
                            .admission_event_watermarks
                            .insert(route.clone(), watermark);
                    }
                    if let Some(watermark) = predecessor_event_watermarks.get(route).copied() {
                        continuation
                            .predecessor_event_watermarks
                            .insert(route.clone(), watermark);
                    }
                    if continuation.admission_event_watermarks.get(route)
                        != continuation.predecessor_event_watermarks.get(route)
                    {
                        continue;
                    }
                    continuation.cover_route(
                        route.clone(),
                        result.clone(),
                        attempt
                            .as_ref()
                            .and_then(|attempt| attempt.receipt.as_ref()),
                    );
                }
            }
            continuation.covered_removed_source_count = attempt
                .as_ref()
                .and_then(|attempt| attempt.receipt.as_ref())
                .map(|receipt| receipt.current.removed_source_count)
                .unwrap_or_default();
            continuation.covered_timings = attempt
                .as_ref()
                .and_then(|attempt| attempt.timings)
                .unwrap_or_default();
        }
        let coverage_certificate = attempt
            .filter(|attempt| {
                publication_ready && attempt.state == SourceBackedRefreshState::Published
            })
            .and_then(|attempt| {
                Some(SourceBackedRefreshCoverageCertificate {
                    request_id: request_id.to_owned(),
                    published_generation: attempt.published_generation.clone()?,
                    routes: certified_routes,
                })
            });
        RouteAdmissionFinish {
            coverage_certificate,
            durable_request_id: request_id.to_owned(),
        }
    }

    fn terminalize_failed_predecessor_demands(
        state: &mut CoreRefreshEngineState,
        predecessor_request_id: &str,
        predecessor: &SourceBackedRefreshAttempt,
    ) -> Option<String> {
        let dependent_request_ids = state
            .manual_all_continuations
            .iter()
            .filter(|(_, continuation)| {
                continuation.predecessor_request_id == predecessor_request_id
            })
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        debug_assert!(
            dependent_request_ids.len() <= 1,
            "one predecessor must have at most one exact broad successor"
        );
        let mut durable_request_id = None;
        for request_id in dependent_request_ids {
            let fallback_routes = find_attempt(state, &request_id)
                .and_then(|attempt| match &attempt.refresh_scope {
                    SourceBackedRefreshScope::All => None,
                    SourceBackedRefreshScope::Exact(routes) => Some(routes.clone()),
                })
                .unwrap_or_default();
            let failure_outcome = predecessor.failure_outcome.clone().unwrap_or_else(|| {
                SourceBackedRefreshFailureOutcome::new(
                    "source_refresh_failed",
                    "internal",
                    true,
                    fallback_routes,
                    Some("retry_request"),
                )
            });
            if let Some(logical) = find_attempt_mut(state, &request_id) {
                logical.state = SourceBackedRefreshState::Failed;
                logical.finished_at_ms = predecessor
                    .finished_at_ms
                    .or_else(|| Some(utc_now().timestamp_millis()));
                logical.published_generation = predecessor.published_generation.clone();
                logical.progress = predecessor.progress.clone();
                logical.progress.phase = "failed".to_owned();
                logical.progress_total_sources_known = predecessor.progress_total_sources_known;
                logical.physical_attempt_id = Some(predecessor_request_id.to_owned());
                logical.failure_type = predecessor.failure_type;
                logical.failure_outcome = Some(failure_outcome);
                logical.last_error = predecessor.last_error.as_ref().map(|detail| {
                    format!("physical predecessor `{predecessor_request_id}` failed: {detail}")
                });
            }
            state.manual_all_continuations.remove(&request_id);
            state.admission_resolutions_in_flight.remove(&request_id);
            state.unacknowledged_admissions.remove(&request_id);
            state.route_admissions.remove(&request_id);
            state.route_admission_watermarks.remove(&request_id);
            state
                .pending_request_ids
                .retain(|pending| pending != &request_id);
            if state.active_request_id.as_deref() == Some(request_id.as_str()) {
                state.active_request_id = state.pending_request_ids.pop_front();
            }
            durable_request_id.get_or_insert(request_id);
        }
        durable_request_id
    }

    pub(super) fn restore_route_dispositions_locked(
        state: &mut CoreRefreshEngineState,
        retryable_routes: &BTreeSet<SourceRouteIdentity>,
        blocked_routes: &BTreeSet<SourceRouteIdentity>,
    ) {
        let routes = retryable_routes
            .union(blocked_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if routes.is_empty() {
            return;
        }
        let now_ms = source_route_ledger_now_ms();
        let watermark = state.dirty_routes.seed_watermark();
        for route in &routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(routes, watermark, now_ms);
        state.dirty_routes.block_exact_routes(blocked_routes.iter());
    }
}
