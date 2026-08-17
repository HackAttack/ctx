use std::{path::Path, time::Duration};

use anyhow::{anyhow, Context};
use ctx_daemon_runtime::daemon_lock_is_active;

use crate::{
    lifecycle, supervisor, DaemonApplicationHost, DaemonHandoff, DaemonStartError,
    DaemonSupervisorReport, DaemonTrigger,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const FORCED_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_RETRY: Duration = Duration::from_millis(50);
const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);
const CONTROL_RESPONSE_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug)]
pub enum DaemonEnabledUpdateError {
    Operation(anyhow::Error),
    StartSuppressed,
    Supervisor(anyhow::Error),
    Start(DaemonStartError),
}

#[derive(Debug)]
pub struct DaemonEnabledUpdate {
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub persistent: bool,
    pub supervisor: DaemonSupervisorReport,
}

pub(super) fn update_daemon_enabled(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    enabled: bool,
) -> Result<DaemonEnabledUpdate, DaemonEnabledUpdateError> {
    host.set_daemon_enabled(data_root, enabled)
        .map_err(DaemonEnabledUpdateError::Operation)?;
    let handoff = if enabled {
        let config = host
            .daemon_config(data_root)
            .map_err(DaemonEnabledUpdateError::Operation)?;
        if lifecycle::daemon_start_is_fenced(host) {
            return Err(DaemonEnabledUpdateError::StartSuppressed);
        }
        if lifecycle::daemon_autostart_suppression_reason().is_none() {
            supervisor::ensure_daemon_supervisor(host, data_root)
                .map_err(DaemonEnabledUpdateError::Supervisor)?;
        }
        Some(
            lifecycle::start_daemon_and_wait(host, data_root, &config, DaemonTrigger::Setup)
                .map_err(DaemonEnabledUpdateError::Start)?,
        )
    } else {
        request_daemon_shutdown_and_wait(host, data_root)
            .map_err(DaemonEnabledUpdateError::Operation)?;
        supervisor::disable_daemon_supervisor(host, data_root)
            .map_err(DaemonEnabledUpdateError::Operation)?;
        host.cancel_core_finalization_generation_lease(data_root, "daemon was disabled")
            .map_err(DaemonEnabledUpdateError::Operation)?;
        None
    };
    let supervisor =
        DaemonSupervisorReport::new(supervisor::daemon_supervisor_report(host, data_root));
    let running = handoff.is_some();
    let persistent = enabled && running;
    Ok(DaemonEnabledUpdate {
        enabled,
        running,
        pid: handoff.map(|handoff: DaemonHandoff| handoff.pid),
        persistent,
        supervisor,
    })
}

fn request_daemon_shutdown_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
) -> anyhow::Result<()> {
    request_daemon_shutdown_and_wait_with(
        || daemon_lock_is_active(data_root),
        |timeout, response_limit| {
            let _ = host.request_daemon_shutdown(data_root, timeout, response_limit);
        },
        || host.terminate_current_executable_daemon(data_root),
        || host.remove_released_daemon_service_artifacts(data_root),
        std::time::Instant::now,
        std::thread::sleep,
        SHUTDOWN_TIMEOUT,
        FORCED_SHUTDOWN_TIMEOUT,
        SHUTDOWN_RETRY,
    )
}

#[allow(clippy::too_many_arguments)]
fn request_daemon_shutdown_and_wait_with(
    mut owner_is_active: impl FnMut() -> bool,
    mut request_shutdown: impl FnMut(Duration, u64),
    mut terminate_owner: impl FnMut() -> anyhow::Result<()>,
    mut cleanup_artifacts: impl FnMut() -> anyhow::Result<()>,
    mut now: impl FnMut() -> std::time::Instant,
    mut sleep: impl FnMut(Duration),
    shutdown_timeout: Duration,
    forced_shutdown_timeout: Duration,
    retry_interval: Duration,
) -> anyhow::Result<()> {
    let mut deadline = now() + shutdown_timeout;
    let mut forced = false;
    while owner_is_active() {
        request_shutdown(CONTROL_TIMEOUT, CONTROL_RESPONSE_MAX_BYTES);
        if now() >= deadline {
            if forced {
                return Err(anyhow!(
                    "daemon was disabled but retained lifecycle ownership after identity-verified termination"
                ));
            }
            terminate_owner().context(
                "terminate identity-verified daemon after cooperative shutdown timed out",
            )?;
            forced = true;
            deadline = now() + forced_shutdown_timeout;
        }
        sleep(retry_interval);
    }
    cleanup_artifacts()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn synthetic_now(
        base: std::time::Instant,
        ticks: &Cell<u64>,
    ) -> impl FnMut() -> std::time::Instant + '_ {
        move || {
            let tick = ticks.get();
            ticks.set(tick + 1);
            base + Duration::from_secs(tick)
        }
    }

    #[test]
    fn owner_free_disable_skips_requests_and_termination_before_cleanup() {
        let requests = Cell::new(0);
        let terminations = Cell::new(0);
        let cleanups = Cell::new(0);
        let sleeps = Cell::new(0);
        let ticks = Cell::new(0);

        request_daemon_shutdown_and_wait_with(
            || false,
            |_, _| requests.set(requests.get() + 1),
            || {
                terminations.set(terminations.get() + 1);
                Ok(())
            },
            || {
                cleanups.set(cleanups.get() + 1);
                Ok(())
            },
            synthetic_now(std::time::Instant::now(), &ticks),
            |_| sleeps.set(sleeps.get() + 1),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .unwrap();

        assert_eq!(requests.get(), 0);
        assert_eq!(terminations.get(), 0);
        assert_eq!(cleanups.get(), 1);
        assert_eq!(sleeps.get(), 0);
        assert_eq!(ticks.get(), 1);
    }

    #[test]
    fn cooperative_shutdown_retries_with_exact_request_bounds() {
        let owner_checks = Cell::new(0);
        let requests = Cell::new(0);
        let cleanups = Cell::new(0);
        let sleeps = Cell::new(0);
        let ticks = Cell::new(0);

        request_daemon_shutdown_and_wait_with(
            || {
                let check = owner_checks.get();
                owner_checks.set(check + 1);
                check < 3
            },
            |timeout, response_limit| {
                assert_eq!(timeout, Duration::from_millis(500));
                assert_eq!(response_limit, 16 * 1024);
                requests.set(requests.get() + 1);
            },
            || panic!("cooperative release must not terminate the owner"),
            || {
                cleanups.set(cleanups.get() + 1);
                Ok(())
            },
            synthetic_now(std::time::Instant::now(), &ticks),
            |duration| {
                assert_eq!(duration, Duration::from_millis(50));
                sleeps.set(sleeps.get() + 1);
            },
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .unwrap();

        assert_eq!(owner_checks.get(), 4);
        assert_eq!(requests.get(), 3);
        assert_eq!(cleanups.get(), 1);
        assert_eq!(sleeps.get(), 3);
        assert_eq!(ticks.get(), 4);
    }

    #[test]
    fn timeout_terminates_once_then_uses_the_forced_window() {
        let owner_checks = Cell::new(0);
        let requests = Cell::new(0);
        let terminations = Cell::new(0);
        let cleanups = Cell::new(0);
        let sleeps = Cell::new(0);
        let ticks = Cell::new(0);

        request_daemon_shutdown_and_wait_with(
            || {
                let check = owner_checks.get();
                owner_checks.set(check + 1);
                check < 1
            },
            |timeout, response_limit| {
                assert_eq!(timeout, CONTROL_TIMEOUT);
                assert_eq!(response_limit, CONTROL_RESPONSE_MAX_BYTES);
                requests.set(requests.get() + 1);
            },
            || {
                terminations.set(terminations.get() + 1);
                Ok(())
            },
            || {
                cleanups.set(cleanups.get() + 1);
                Ok(())
            },
            synthetic_now(std::time::Instant::now(), &ticks),
            |_| sleeps.set(sleeps.get() + 1),
            Duration::ZERO,
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .unwrap();

        assert_eq!(owner_checks.get(), 2);
        assert_eq!(requests.get(), 1);
        assert_eq!(terminations.get(), 1);
        assert_eq!(cleanups.get(), 1);
        assert_eq!(sleeps.get(), 1);
        assert_eq!(ticks.get(), 3);
    }

    #[test]
    fn retained_owner_after_forced_window_is_a_stable_failure() {
        let requests = Cell::new(0);
        let terminations = Cell::new(0);
        let cleanups = Cell::new(0);
        let sleeps = Cell::new(0);
        let ticks = Cell::new(0);

        let error = request_daemon_shutdown_and_wait_with(
            || true,
            |_, _| requests.set(requests.get() + 1),
            || {
                terminations.set(terminations.get() + 1);
                Ok(())
            },
            || {
                cleanups.set(cleanups.get() + 1);
                Ok(())
            },
            synthetic_now(std::time::Instant::now(), &ticks),
            |_| sleeps.set(sleeps.get() + 1),
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(50),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "daemon was disabled but retained lifecycle ownership after identity-verified termination"
        );
        assert_eq!(requests.get(), 2);
        assert_eq!(terminations.get(), 1);
        assert_eq!(cleanups.get(), 0);
        assert_eq!(sleeps.get(), 1);
        assert_eq!(ticks.get(), 4);
    }

    #[test]
    fn identity_termination_failure_keeps_its_exact_context_and_skips_cleanup() {
        let cleanups = Cell::new(0);
        let ticks = Cell::new(0);

        let error = request_daemon_shutdown_and_wait_with(
            || true,
            |_, _| {},
            || Err(anyhow!("identity mismatch")),
            || {
                cleanups.set(cleanups.get() + 1);
                Ok(())
            },
            synthetic_now(std::time::Instant::now(), &ticks),
            |_| {},
            Duration::ZERO,
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "terminate identity-verified daemon after cooperative shutdown timed out"
        );
        assert_eq!(error.root_cause().to_string(), "identity mismatch");
        assert_eq!(cleanups.get(), 0);
    }
}
