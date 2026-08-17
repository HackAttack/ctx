use super::*;

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
