use super::*;

/// Process-local admission boundary for an explicitly finite daemon.
///
/// A successful explicit source-refresh demand closes automatic provider-work
/// admission for the remainder of this daemon lifecycle. Already admitted
/// explicit successors still run before this policy is consulted, and watcher
/// observations remain pending until shutdown; startup reconciliation observes
/// the provider again in the next lifecycle. Persistent daemons deliberately
/// ignore the closed boundary.
#[derive(Debug, Default)]
pub(crate) struct FiniteRefreshAdmissionEpoch {
    explicit_demand_converged: bool,
}

impl FiniteRefreshAdmissionEpoch {
    pub(crate) fn observe_terminal(&mut self, job: &Value, failed: bool) {
        if !failed
            && job.get("status").and_then(Value::as_str) == Some("completed")
            && job.get("request_state").and_then(Value::as_str) == Some("published")
            && daemon_job_is_explicit_source_refresh(job)
        {
            self.explicit_demand_converged = true;
        }
    }

    pub(crate) fn allows_automatic_provider_refresh(&self, idle_exit: Option<StdDuration>) -> bool {
        idle_exit.is_none() || !self.explicit_demand_converged
    }
}

fn daemon_job_is_explicit_source_refresh(job: &Value) -> bool {
    matches!(
        (
            job.get("operation").and_then(Value::as_str),
            job.get("trigger").and_then(Value::as_str),
            job.get("trigger_provenance").and_then(Value::as_str),
        ),
        (Some("import"), _, _)
            | (
                Some("refresh"),
                Some("search"),
                Some("manual" | "autostart")
            )
    )
}

pub(super) fn daemon_should_schedule_auto_upgrade(
    daemon_enabled: bool,
    daemon_mode: DaemonMode,
) -> bool {
    daemon_enabled && daemon_mode == DaemonMode::Full
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fail_daemon_before_ready_for_test(data_root: &Path) -> Result<()> {
    if data_root
        .join(".fail-daemon-before-ready-for-test")
        .exists()
    {
        return Err(anyhow!("injected daemon failure before readiness"));
    }
    Ok(())
}

pub(super) fn daemon_services_can_begin_idle_shutdown(
    query_service: Option<&DaemonQueryService>,
    observed_query_generation: u64,
    refresh_service: Option<&DaemonQueryService>,
    observed_refresh_generation: u64,
) -> bool {
    let refresh_activity = refresh_service.map(|service| service.activity.as_ref());
    if !daemon_can_begin_idle_shutdown(refresh_activity, observed_refresh_generation) {
        return false;
    }
    if daemon_can_begin_idle_shutdown(
        query_service.map(|service| service.activity.as_ref()),
        observed_query_generation,
    ) {
        return true;
    }
    if let Some(activity) = refresh_activity {
        activity.resume_accepting();
    }
    false
}

pub(super) fn ensure_daemon_ipc_services_healthy(
    query_service: Option<&DaemonQueryService>,
    refresh_service: Option<&DaemonQueryService>,
) -> Result<()> {
    for service in [refresh_service, query_service].into_iter().flatten() {
        if service.listener_finished() {
            return Err(anyhow!(
                "daemon {} IPC listener exited unexpectedly",
                service.service_id().as_str()
            ));
        }
    }
    Ok(())
}

pub(super) fn daemon_should_attempt_finite_idle_shutdown(
    idle_exit: Option<StdDuration>,
    idle_since: Option<Instant>,
    _retry_due: bool,
    _source_refresh_pending: bool,
) -> bool {
    idle_exit.is_some_and(|limit| idle_since.is_some_and(|idle| idle.elapsed() >= limit))
}
