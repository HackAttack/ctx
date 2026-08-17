use super::*;

impl CoreRefreshEngine {
    pub fn status(&self, request_id: &str) -> Option<RefreshStatus> {
        let state = self.lock_state();
        projected_status_json(&state, request_id).map(RefreshStatus::from_schema_v1_fields)
    }

    pub(super) fn requested_explicit_source_catalog(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone())
    }

    pub(super) fn refresh_scope(&self, request_id: &str) -> Option<SourceBackedRefreshScope> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.refresh_scope.clone())
    }

    pub(super) fn operation(&self, request_id: &str) -> Option<SourceBackedRefreshOperation> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.operation)
    }

    pub(super) fn reconciliation_demand(
        &self,
        request_id: &str,
    ) -> Option<SourceBackedReconciliationDemand> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.reconciliation_demand)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn request_catalog_authority_for_test(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone())
    }

    pub(super) fn admit_refresh_scope(
        &self,
        request_id: &str,
        scope: &SourceBackedRefreshScope,
    ) -> Result<AdmittedRefreshScope> {
        let now_ms = source_route_ledger_now_ms();
        let mut state = self.lock_state();
        if state.route_admissions.contains_key(request_id) {
            bail!("source refresh request `{request_id}` already has retained route admissions");
        }
        let (admitted_authority, requires_admitted_discovery) = find_attempt(&state, request_id)
            .map(|attempt| {
                (
                    attempt.admitted_authority.clone(),
                    attempt.selector.is_scoped()
                        && matches!(scope, SourceBackedRefreshScope::Exact(_)),
                )
            })
            .unwrap_or((None, false));
        if let Some(authority) = admitted_authority.as_ref() {
            if &authority.scope != scope {
                bail!("scoped source refresh execution does not match its admitted exact scope");
            }
        }
        let known_route_ids = state.known_route_ids.clone();
        let mut covered_route_ids = if let Some(continuation) =
            state.manual_all_continuations.get_mut(request_id)
        {
            if !continuation.predecessor_finished {
                bail!(
                    "manual all-route continuation `{request_id}` started before its exact predecessor finished"
                );
            }
            let retained = continuation
                .covered_route_results
                .keys()
                .filter(|route| known_route_ids.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>();
            if retained.len() != continuation.covered_route_results.len() {
                continuation
                    .covered_route_results
                    .retain(|route, _| retained.contains(route));
                if continuation.covered_route_results.is_empty() {
                    continuation.covered_removed_source_count = 0;
                    continuation.covered_timings = SourceBackedRefreshTimings::default();
                }
            }
            retained
        } else {
            BTreeSet::new()
        };
        let admissions = match scope {
            SourceBackedRefreshScope::All => {
                let watermark = state.dirty_routes.seed_watermark();
                let routes = known_route_ids
                    .difference(&covered_route_ids)
                    .cloned()
                    .collect::<Vec<_>>();
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
                let admissions = state.dirty_routes.admit_all();
                if admissions
                    .iter()
                    .any(|admission| covered_route_ids.contains(admission.route()))
                {
                    covered_route_ids.clear();
                    if let Some(continuation) = state.manual_all_continuations.get_mut(request_id) {
                        continuation.covered_route_results.clear();
                        continuation.covered_removed_source_count = 0;
                        continuation.covered_timings = SourceBackedRefreshTimings::default();
                    }
                }
                admissions
            }
            SourceBackedRefreshScope::Exact(routes) => {
                if routes.is_empty() || routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
                    bail!(
                        "daemon exact source refresh must admit between one and {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
                    );
                }
                if admitted_authority.is_some() {
                    let watermark = state.dirty_routes.seed_watermark();
                    for route in routes {
                        state
                            .route_event_watermarks
                            .entry(route.clone())
                            .and_modify(|current| *current = (*current).max(watermark))
                            .or_insert(watermark);
                    }
                    state.dirty_routes.seed_exact_routes(
                        routes.iter().cloned(),
                        watermark,
                        // A logical request is explicit admission, so its
                        // first exact attempt bypasses watcher debounce.
                        now_ms.saturating_sub(1_000),
                    );
                }
                state
                    .dirty_routes
                    .admit_exact_routes(routes, now_ms)
                    .ok_or_else(|| {
                        anyhow!("one or more exact source routes are no longer due for admission")
                    })?
            }
        };
        state
            .route_admissions
            .insert(request_id.to_owned(), admissions);
        let admitted_watermarks = state
            .route_admissions
            .get(request_id)
            .into_iter()
            .flatten()
            .filter_map(|admission| {
                state
                    .route_event_watermarks
                    .get(admission.route())
                    .copied()
                    .map(|watermark| (admission.route().clone(), watermark))
            })
            .collect::<BTreeMap<_, _>>();
        for continuation in state.manual_all_continuations.values_mut() {
            if continuation.predecessor_request_id == request_id {
                continuation.predecessor_event_watermarks = admitted_watermarks.clone();
            }
        }
        state
            .route_admission_watermarks
            .insert(request_id.to_owned(), admitted_watermarks);
        let covered_publication = state
            .manual_all_continuations
            .get(request_id)
            .map(ManualAllContinuation::covered_publication)
            .unwrap_or_default();
        let incremental_exact = find_attempt(&state, request_id).is_some_and(|attempt| {
            attempt.reconciliation_demand == SourceBackedReconciliationDemand::Incremental
                && matches!(scope, SourceBackedRefreshScope::Exact(_))
        });
        let admitted_routes = state
            .route_admissions
            .get(request_id)
            .into_iter()
            .flatten()
            .map(|admission| admission.route().clone())
            .collect::<Vec<_>>();
        let mut route_worksets = BTreeMap::new();
        for route in admitted_routes {
            if let Some(workset) = state.route_worksets.remove(&route) {
                if incremental_exact {
                    route_worksets.insert(route, workset);
                }
            }
        }
        Ok(AdmittedRefreshScope {
            covered_route_ids,
            covered_publication,
            route_worksets,
            watch_catalog: admitted_authority
                .as_ref()
                .map(|authority| authority.discovery.watch_catalog().clone())
                .or_else(|| state.watch_catalog.clone()),
            admitted_discovery: admitted_authority.map(|authority| authority.discovery),
            requires_admitted_discovery,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn admit_refresh_scope_for_test(
        &self,
        request_id: &str,
        scope: &SourceBackedRefreshScope,
    ) -> Result<BTreeSet<SourceRouteIdentity>> {
        self.admit_refresh_scope(request_id, scope)
            .map(|admitted| admitted.covered_route_ids)
    }
}
