use super::*;
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SourceBackedRefreshFailureOutcome {
    pub(super) code: &'static str,
    pub(super) class: &'static str,
    pub(super) retryable: bool,
    pub(super) affected_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retryable_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) blocked_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retry_advice: Option<&'static str>,
}

impl SourceBackedRefreshFailureOutcome {
    pub(super) fn new(
        code: &'static str,
        class: &'static str,
        retryable: bool,
        affected_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<&'static str>,
    ) -> Self {
        let (retryable_routes, blocked_routes) = if retryable {
            (affected_routes.clone(), BTreeSet::new())
        } else {
            (BTreeSet::new(), affected_routes.clone())
        };
        Self::with_route_dispositions(
            code,
            class,
            retryable,
            retryable_routes,
            blocked_routes,
            retry_advice,
        )
    }

    pub(super) fn with_route_dispositions(
        code: &'static str,
        class: &'static str,
        retryable: bool,
        retryable_routes: BTreeSet<SourceRouteIdentity>,
        blocked_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<&'static str>,
    ) -> Self {
        let affected_routes = retryable_routes.union(&blocked_routes).cloned().collect();
        Self {
            code,
            class,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            retry_advice,
        }
    }

    fn to_json(
        &self,
        physical_attempt_id: &str,
        retained_generation: Option<&str>,
        detail: Option<&str>,
    ) -> Value {
        compact_json(json!({
            "code": self.code,
            "class": self.class,
            "retryable": self.retryable,
            "affected_routes": self.affected_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "retryable_routes": self.retryable_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "blocked_routes": self.blocked_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "physical_attempt_id": physical_attempt_id,
            "retained_generation": retained_generation,
            "retry_advice": self.retry_advice,
            "detail": detail,
        }))
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedRefreshAttempt {
    pub(super) request_id: String,
    pub(super) state: SourceBackedRefreshState,
    pub(super) requested_at_ms: i64,
    pub(super) started_at_ms: Option<i64>,
    pub(super) finished_at_ms: Option<i64>,
    pub(super) previous_generation: Option<String>,
    pub(super) published_generation: Option<String>,
    pub(super) refresh_scope: SourceBackedRefreshScope,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) reconciliation_demand: SourceBackedReconciliationDemand,
    pub(super) requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) fresh_after_admitted_snapshot: bool,
    pub(super) request_fingerprint: Option<String>,
    pub(super) admission_durability_indeterminate: bool,
    pub(super) coalesced_into_request_id: Option<String>,
    pub(super) coalesced_logical_demands: u64,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    pub(super) progress_total_sources_known: bool,
    pub(super) whole_run_eta: WholeRunEtaEstimator,
    pub(super) physical_attempt_id: Option<String>,
    pub(super) scanned_routes: Option<usize>,
    pub(super) unsupported_routes: Option<usize>,
    pub(super) request_source_count: Option<usize>,
    pub(super) certified_source_count: Option<usize>,
    pub(super) certified_source_bytes: Option<u64>,
    /// Request-scoped route/result/rejection facts. This is mutable daemon
    /// status, never publication authority.
    pub(super) receipt: Option<SourceBackedRefreshReceipt>,
    /// The sole publication receipt, decoded from Core CommitPayload metadata.
    pub(super) publication_receipt: Option<SourceBackedRefreshReceipt>,
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
    pub(super) timings: Option<SourceBackedRefreshTimings>,
    pub(super) publication_probe_us: u64,
    pub(super) daemon_mode: String,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<&'static str>,
    pub(super) failure_outcome: Option<SourceBackedRefreshFailureOutcome>,
    pub(super) last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn source_count(&self) -> usize {
        self.request_source_count
            .or(self.scanned_routes)
            .unwrap_or(self.progress.total_sources)
    }

    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
            .or_else(|| self.failure_outcome.as_ref().map(|outcome| outcome.code))
    }

    fn failure_reason(&self) -> Option<&'static str> {
        if self.failure_code() == Some(TERMINAL_COVERAGE_ERROR_CODE) {
            return Some("provider_terminal_coverage_unavailable");
        }
        self.failure_outcome.as_ref().map(|outcome| outcome.class)
    }

    fn request_generation_changed(&self) -> Option<bool> {
        self.receipt
            .as_ref()
            .map(|_| self.published_generation != self.previous_generation)
    }

    fn request_outcome_receipt(&self) -> Option<&SourceBackedRefreshReceipt> {
        let request = self.receipt.as_ref()?;
        self.publication_receipt
            .as_ref()
            .filter(|publication| *publication != request)
            .map(|_| request)
    }

    fn default_logical_phase(&self) -> &'static str {
        match self.state {
            SourceBackedRefreshState::Published | SourceBackedRefreshState::Failed => "terminal",
            SourceBackedRefreshState::Running => "direct",
            SourceBackedRefreshState::AdmissionPending | SourceBackedRefreshState::Queued => {
                "waiting"
            }
        }
    }

    fn physical_attempt_id(&self) -> &str {
        self.physical_attempt_id
            .as_deref()
            .unwrap_or(self.request_id.as_str())
    }

    fn structured_outcome_json(&self) -> Option<Value> {
        if let Some(receipt) = self.receipt.as_ref() {
            let code = receipt.terminal_outcome();
            let (retryable_routes, blocked_routes) = receipt.route_retry_dispositions();
            let retryable = !retryable_routes.is_empty();
            let affected_routes = receipt
                .route_results
                .iter()
                .filter(|result| {
                    result.outcome.is_failure()
                        || result.source_failure_total != 0
                        || result.rejected_record_total != 0
                })
                .map(|result| result.route_identity.as_str())
                .collect::<Vec<_>>();
            return Some(compact_json(json!({
                "code": code,
                "class": if retryable {
                    "completed_with_retryable_failures"
                } else if code == "completed" {
                    "completed"
                } else {
                    "completed_with_diagnostics"
                },
                "retryable": retryable,
                "affected_routes": affected_routes,
                "retryable_routes": retryable_routes
                    .iter()
                    .map(SourceRouteIdentity::as_str)
                    .collect::<Vec<_>>(),
                "blocked_routes": blocked_routes
                    .iter()
                    .map(SourceRouteIdentity::as_str)
                    .collect::<Vec<_>>(),
                "physical_attempt_id": self.physical_attempt_id(),
                "retained_generation": (code != "completed" || !receipt.generation_changed)
                    .then_some(receipt.published_generation.as_str()),
                "published_generation": receipt.published_generation,
                "retry_advice": retryable.then_some("retry_affected_routes"),
            })));
        }
        self.failure_outcome.as_ref().map(|outcome| {
            outcome.to_json(
                self.physical_attempt_id(),
                self.published_generation.as_deref(),
                self.last_error.as_deref(),
            )
        })
    }

    fn apply_base_read_fields(&self, mut value: Value) -> Value {
        let Some(fields) = value.as_object_mut() else {
            return value;
        };
        fields.insert("logical_request_id".to_owned(), json!(self.request_id));
        fields.insert(
            "logical_phase".to_owned(),
            json!(self.default_logical_phase()),
        );
        fields.insert(
            "physical_attempt_id".to_owned(),
            json!(self.physical_attempt_id()),
        );
        fields.insert(
            "physical_attempt_state".to_owned(),
            json!(self.state.as_str()),
        );
        fields.insert(
            "progress_owner_request_id".to_owned(),
            json!(self.request_id),
        );
        fields.insert(
            "progress_owner_attempt_state".to_owned(),
            json!(self.state.as_str()),
        );
        fields.insert(
            "reconciliation_demand".to_owned(),
            json!(self.reconciliation_demand.as_str()),
        );
        if let Some(outcome) = self.structured_outcome_json() {
            fields.insert("structured_outcome".to_owned(), outcome);
        }
        value
    }

    pub(super) fn to_json(&self) -> Value {
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        self.apply_base_read_fields(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.receipt.is_none().then(|| {
                self.requested_explicit_source_catalog
                    .as_ref()
                    .map(ExplicitSourceCatalogAuthority::to_json)
            }).flatten(),
            "fresh_after_admitted_snapshot": self.fresh_after_admitted_snapshot,
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": self.coalesced_into_request_id,
            "coalesced_logical_demands": self.coalesced_logical_demands,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json_with_total_known(
                self.progress_total_sources_known,
                self.whole_run_eta.estimated_remaining_millis(),
            ),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
        })))
    }

    pub(super) fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::AdmissionPending
            | SourceBackedRefreshState::Queued
            | SourceBackedRefreshState::Running => "running",
        };
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        self.apply_base_read_fields(compact_json(json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "core_refresh",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation.as_str(),
            "source_count": self.source_count(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.receipt.is_none().then(|| {
                self.requested_explicit_source_catalog
                    .as_ref()
                    .map(ExplicitSourceCatalogAuthority::to_json)
            }).flatten(),
            "fresh_after_admitted_snapshot": self.fresh_after_admitted_snapshot,
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": self.coalesced_into_request_id,
            "coalesced_logical_demands": self.coalesced_logical_demands,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json_with_total_known(
                self.progress_total_sources_known,
                self.whole_run_eta.estimated_remaining_millis(),
            ),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
        })))
    }

    fn timings_json(&self) -> Option<Value> {
        self.timings.map(|timings| {
            let mut timings = timings.to_json();
            timings["publication_probe"] = json!(self.publication_probe_us);
            timings
        })
    }
}

pub(super) fn projected_status_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    Some(apply_read_projection(
        state,
        attempt,
        attempt.to_json(),
        false,
    ))
}

pub(super) fn projected_job_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    Some(apply_read_projection(
        state,
        attempt,
        attempt.job_json(),
        true,
    ))
}

fn apply_read_projection(
    state: &CoreRefreshEngineState,
    logical: &SourceBackedRefreshAttempt,
    mut value: Value,
    job: bool,
) -> Value {
    let continuation = state.manual_all_continuations.get(&logical.request_id);
    let logical_phase = if !logical.state.is_active() {
        "terminal"
    } else if state
        .admission_resolutions_in_flight
        .contains(&logical.request_id)
    {
        "coverage_check"
    } else if let Some(continuation) = continuation {
        if !continuation.predecessor_finished {
            "attached"
        } else if continuation.is_fully_covered() {
            "coverage_check"
        } else if logical.state == SourceBackedRefreshState::Running {
            "exact_successor"
        } else {
            "waiting"
        }
    } else {
        logical.default_logical_phase()
    };

    let progress_owner = continuation
        .filter(|continuation| {
            logical.state.is_active()
                && (!continuation.predecessor_finished || continuation.is_fully_covered())
        })
        .and_then(|continuation| find_attempt(state, &continuation.predecessor_request_id))
        .unwrap_or(logical);
    let physical_attempt_id = if continuation.is_some_and(|continuation| {
        logical.state.is_active()
            && continuation.predecessor_finished
            && !continuation.is_fully_covered()
    }) {
        logical.request_id.as_str()
    } else {
        logical.physical_attempt_id.as_deref().unwrap_or_else(|| {
            continuation
                .map(|continuation| continuation.predecessor_request_id.as_str())
                .unwrap_or(logical.request_id.as_str())
        })
    };
    let physical_state = find_attempt(state, physical_attempt_id)
        .map(|attempt| attempt.state)
        .unwrap_or(logical.state);

    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    fields.insert("logical_phase".to_owned(), json!(logical_phase));
    fields.insert("physical_attempt_id".to_owned(), json!(physical_attempt_id));
    fields.insert(
        "physical_attempt_state".to_owned(),
        json!(physical_state.as_str()),
    );
    fields.insert(
        "progress_owner_request_id".to_owned(),
        json!(progress_owner.request_id),
    );
    fields.insert(
        "progress_owner_attempt_state".to_owned(),
        json!(progress_owner.state.as_str()),
    );
    fields.insert(
        "progress".to_owned(),
        progress_owner.progress.to_json_with_total_known(
            progress_owner.progress_total_sources_known,
            progress_owner.whole_run_eta.estimated_remaining_millis(),
        ),
    );
    if job {
        fields.insert(
            "source_count".to_owned(),
            json!(progress_owner.source_count()),
        );
    }
    if let Some(outcome) = fields
        .get_mut("structured_outcome")
        .and_then(Value::as_object_mut)
    {
        outcome.insert("physical_attempt_id".to_owned(), json!(physical_attempt_id));
    }
    value
}
