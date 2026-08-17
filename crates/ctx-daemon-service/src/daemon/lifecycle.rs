use super::*;

const ON_DEMAND_REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const ON_DEMAND_QUIET_GRACE: StdDuration = StdDuration::from_millis(300);

/// Bounds one on-demand daemon process around explicit IPC refresh demand.
#[derive(Debug)]
pub(super) struct OnDemandExit {
    started_at: Instant,
    demand_observed: bool,
    idle_since: Option<Instant>,
    observed_ipc_activity_generation: u64,
    observed_request_activity_generation: u64,
}

impl OnDemandExit {
    pub(super) fn new(refresh_service: Option<&DaemonQueryService>) -> Self {
        Self {
            started_at: Instant::now(),
            demand_observed: false,
            idle_since: None,
            observed_ipc_activity_generation: refresh_service
                .map(|service| service.activity.snapshot().1)
                .unwrap_or(0),
            // The engine is process-local and starts at zero. Preserve any
            // request admitted during startup so the first loop observation
            // still recognizes it as demand even if it already completed.
            observed_request_activity_generation: 0,
        }
    }

    pub(super) fn observe(
        &mut self,
        source_refresh: Option<&CoreRefreshEngine>,
        refresh_service: Option<&DaemonQueryService>,
        now: Instant,
    ) -> bool {
        let pending = source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
        let (active, ipc_generation) = refresh_service
            .map(|service| service.activity.snapshot())
            .unwrap_or((0, self.observed_ipc_activity_generation));
        let request_generation = source_refresh
            .map(|engine| engine.request_activity_generation())
            .unwrap_or(self.observed_request_activity_generation);
        self.observe_state(pending, active, ipc_generation, request_generation, now)
    }

    fn observe_state(
        &mut self,
        pending: bool,
        active: usize,
        ipc_generation: u64,
        request_generation: u64,
        now: Instant,
    ) -> bool {
        let ipc_activity_changed = ipc_generation != self.observed_ipc_activity_generation;
        self.observed_ipc_activity_generation = ipc_generation;
        let request_activity_changed =
            request_generation != self.observed_request_activity_generation;
        self.observed_request_activity_generation = request_generation;
        if pending || request_activity_changed {
            self.demand_observed = true;
            self.idle_since = None;
            return false;
        }
        if !self.demand_observed {
            return now.saturating_duration_since(self.started_at) >= ON_DEMAND_REQUEST_TIMEOUT;
        }
        if active > 0 || ipc_activity_changed {
            self.idle_since = None;
            return false;
        }
        let idle_since = self.idle_since.get_or_insert(now);
        now.saturating_duration_since(*idle_since) >= ON_DEMAND_QUIET_GRACE
    }

    pub(super) fn wait_duration(&self, now: Instant) -> StdDuration {
        if self.demand_observed {
            self.idle_since.map_or(ON_DEMAND_QUIET_GRACE, |idle| {
                ON_DEMAND_QUIET_GRACE.saturating_sub(now.saturating_duration_since(idle))
            })
        } else {
            ON_DEMAND_REQUEST_TIMEOUT.saturating_sub(now.saturating_duration_since(self.started_at))
        }
    }
}

pub(super) fn daemon_should_schedule_auto_upgrade(
    persistent: bool,
    daemon_mode: DaemonMode,
) -> bool {
    persistent && daemon_mode == DaemonMode::Full
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(started_at: Instant) -> OnDemandExit {
        OnDemandExit {
            started_at,
            demand_observed: false,
            idle_since: None,
            observed_ipc_activity_generation: 0,
            observed_request_activity_generation: 0,
        }
    }

    #[test]
    fn exits_if_no_request_arrives_within_admission_timeout() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(false, 0, 0, 0, started_at));
        assert!(exit.observe_state(false, 0, 0, 0, started_at + ON_DEMAND_REQUEST_TIMEOUT));
    }

    #[test]
    fn exits_only_after_observed_demand_finishes_and_quiets() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(true, 0, 1, 1, started_at));
        assert!(!exit.observe_state(false, 1, 2, 1, started_at + StdDuration::from_millis(1)));
        let idle = started_at + StdDuration::from_millis(2);
        assert!(!exit.observe_state(false, 0, 3, 1, idle));
        assert!(!exit.observe_state(false, 0, 3, 1, idle + ON_DEMAND_QUIET_GRACE));
        assert!(exit.observe_state(
            false,
            0,
            3,
            1,
            idle + ON_DEMAND_QUIET_GRACE + ON_DEMAND_QUIET_GRACE,
        ));
    }

    #[test]
    fn completed_request_activity_is_still_observed_as_demand() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(false, 0, 2, 1, started_at));
        assert!(exit.demand_observed);
        assert!(!exit.observe_state(false, 0, 2, 1, started_at + ON_DEMAND_QUIET_GRACE));
        assert!(exit.observe_state(
            false,
            0,
            2,
            1,
            started_at + ON_DEMAND_QUIET_GRACE + ON_DEMAND_QUIET_GRACE
        ));
    }
}
