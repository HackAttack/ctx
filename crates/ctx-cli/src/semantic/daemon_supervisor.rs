//! CLI composition adapter for neutral daemon-supervisor policy.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use serde_json::Value;

pub(super) use ctx_daemon_application::{
    DaemonSupervisorStart, DaemonSupervisorUpgradeFence, DaemonSupervisorUpgradeResume,
};

pub(super) struct CliDaemonApplicationHost;

impl ctx_daemon_application::DaemonApplicationHost for CliDaemonApplicationHost {
    fn hosted_uninstall_active(&self) -> Result<bool> {
        ctx_upgrade_engine::installation_hosted_uninstall_is_active()
    }

    fn hosted_uninstall_active_for_executable(&self, executable: &Path) -> Result<bool> {
        ctx_upgrade_engine::installation_hosted_uninstall_is_active_for_executable(executable)
    }

    fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
        ctx_upgrade_engine::managed_install_executable()
    }

    fn installation_upgrade_active(&self) -> Result<bool> {
        ctx_upgrade_engine::installation_upgrade_is_active()
    }

    fn daemon_upgrade_handoff_fences_start(&self, data_root: &Path) -> bool {
        super::daemon_autostart::daemon_upgrade_handoff_fences_start(data_root)
    }

    fn daemon_config(
        &self,
        data_root: &Path,
    ) -> Result<ctx_daemon_application::DaemonConfigSnapshot> {
        let config = crate::config::AppConfig::load(data_root)?;
        Ok(ctx_daemon_application::DaemonConfigSnapshot {
            enabled: config.daemon.enabled,
            mode: daemon_mode(config.daemon.mode),
            semantic_enabled: config.semantic_search_enabled(),
        })
    }

    fn write_restart_request(
        &self,
        data_root: &Path,
        trigger: ctx_daemon_application::DaemonTrigger,
    ) -> Result<PathBuf> {
        super::daemon_autostart::write_daemon_restart_request(
            data_root,
            daemon_trigger_arg(trigger),
            &uuid::Uuid::now_v7().to_string(),
        )
    }

    fn request_lifecycle_wakeup(
        &self,
        data_root: &Path,
        request: Value,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<Option<Value>> {
        super::query_service::daemon_source_refresh_request(
            data_root,
            request,
            timeout,
            response_limit,
        )
    }

    fn home_dir(&self) -> Option<PathBuf> {
        crate::identity::home_dir()
    }
}

pub(super) const fn daemon_trigger_arg(
    trigger: ctx_daemon_application::DaemonTrigger,
) -> crate::DaemonTriggerCommandArg {
    match trigger {
        ctx_daemon_application::DaemonTrigger::Setup => crate::DaemonTriggerCommandArg::Setup,
        ctx_daemon_application::DaemonTrigger::Import => crate::DaemonTriggerCommandArg::Import,
        ctx_daemon_application::DaemonTrigger::Search => crate::DaemonTriggerCommandArg::Search,
    }
}

pub(super) const fn daemon_trigger(
    trigger: crate::DaemonTriggerCommandArg,
) -> ctx_daemon_application::DaemonTrigger {
    match trigger {
        crate::DaemonTriggerCommandArg::Setup => ctx_daemon_application::DaemonTrigger::Setup,
        crate::DaemonTriggerCommandArg::Import => ctx_daemon_application::DaemonTrigger::Import,
        crate::DaemonTriggerCommandArg::Search => ctx_daemon_application::DaemonTrigger::Search,
    }
}

pub(super) const fn daemon_mode(
    mode: crate::config::DaemonMode,
) -> ctx_daemon_application::DaemonMode {
    match mode {
        crate::config::DaemonMode::Full => ctx_daemon_application::DaemonMode::Full,
        crate::config::DaemonMode::SourceRefreshOnly => {
            ctx_daemon_application::DaemonMode::SourceRefreshOnly
        }
    }
}

pub(super) fn with_daemon_application<T>(
    operation: impl FnOnce(&ctx_daemon_application::DaemonApplication<'_>) -> T,
) -> T {
    let host = CliDaemonApplicationHost;
    let application = ctx_daemon_application::DaemonApplication::new(&host);
    operation(&application)
}

pub(super) fn ensure_daemon_supervisor(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    data_root: &Path,
) -> Result<DaemonSupervisorStart> {
    application.ensure_daemon_supervisor(data_root)
}

pub(super) fn disable_daemon_supervisor(data_root: &Path) -> Result<()> {
    with_daemon_application(|application| application.disable_daemon_supervisor(data_root))
}

pub(in crate::semantic) fn daemon_supervisor_report(data_root: &Path) -> Value {
    with_daemon_application(|application| application.daemon_supervisor_report(data_root))
}

pub(super) fn resume_daemon_supervisor_after_upgrade(
    data_root: &Path,
    executable: &Path,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    with_daemon_application(|application| {
        application.resume_daemon_supervisor_after_upgrade(data_root, executable, upgrade_fence)
    })
}
