//! CLI composition adapter for neutral daemon-supervisor policy.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use ctx_client_observability::analytics::PublicEventV1;
use ctx_daemon_service::DaemonObservationPort as _;
use serde_json::Value;

pub(super) use ctx_daemon_application::{
    DaemonSupervisorStart, DaemonSupervisorUpgradeFence, DaemonSupervisorUpgradeResume,
};

pub(super) struct CliDaemonApplicationHost<'a> {
    run_config: Option<&'a crate::config::AppConfig>,
}

impl CliDaemonApplicationHost<'_> {
    const fn new() -> Self {
        Self { run_config: None }
    }

    const fn for_daemon_run(config: &crate::config::AppConfig) -> CliDaemonApplicationHost<'_> {
        CliDaemonApplicationHost {
            run_config: Some(config),
        }
    }
}

impl ctx_daemon_application::DaemonApplicationHost for CliDaemonApplicationHost<'_> {
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

    fn run_daemon_service(
        &self,
        data_root: &Path,
        request: ctx_daemon_application::DaemonHostRunRequest,
    ) -> Result<()> {
        use ctx_daemon_service::{
            DaemonRunArgs, DaemonStartMode, DaemonSupervisor, DaemonTrigger, DaemonUpgradePorts,
        };

        let config = self
            .run_config
            .ok_or_else(|| anyhow::anyhow!("daemon run host is missing its borrowed config"))?;
        let service_args = DaemonRunArgs {
            idle_exit_seconds: request.idle_exit_seconds,
            loop_interval_seconds: request.loop_interval_seconds,
            max_chunks: request.max_chunks,
            max_seconds: None,
            force: request.force,
            start_mode: request.start_mode.map(|mode| match mode {
                ctx_daemon_application::DaemonHostStartMode::Manual => DaemonStartMode::Manual,
                ctx_daemon_application::DaemonHostStartMode::Auto => DaemonStartMode::Auto,
            }),
            trigger_command: request.trigger.map(|trigger| match trigger {
                ctx_daemon_application::DaemonTrigger::Setup => DaemonTrigger::Setup,
                ctx_daemon_application::DaemonTrigger::Import => DaemonTrigger::Import,
                ctx_daemon_application::DaemonTrigger::Search => DaemonTrigger::Search,
            }),
            supervisor: if matches!(
                request.start_mode,
                Some(ctx_daemon_application::DaemonHostStartMode::Auto)
            ) && super::health_search::semantic_env_flag(
                super::runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
            ) {
                DaemonSupervisor::CliAutostart
            } else {
                DaemonSupervisor::User
            },
        };
        let engine = crate::upgrade::ports::engine();
        let upgrade = DaemonUpgradePorts {
            engine: &engine,
            daemon: &crate::upgrade::ports::DAEMON_UPGRADE,
            automatic_policy: &crate::upgrade::ports::AUTOMATIC_POLICY,
            observer: &crate::upgrade::ports::UPGRADE_OBSERVER,
        };
        ctx_daemon_service::run_daemon(
            service_args,
            data_root,
            super::daemon_service_ports::config_snapshot(config),
            &super::daemon_service_ports::PORTS,
            &upgrade,
        )
    }

    fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
        crate::config::set_daemon_enabled(data_root, enabled)
    }

    fn request_daemon_shutdown(
        &self,
        data_root: &Path,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<()> {
        super::query_service::daemon_source_refresh_request(
            data_root,
            crate::compact_json(serde_json::json!({
                "schema_version": 1,
                "op": "shutdown",
            })),
            timeout,
            response_limit,
        )
        .map(|_| ())
    }

    fn terminate_current_executable_daemon(&self, data_root: &Path) -> Result<()> {
        super::daemon_autostart::terminate_current_executable_daemon(data_root)
    }

    fn remove_released_daemon_service_artifacts(&self, data_root: &Path) -> Result<()> {
        super::daemon::control::remove_released_daemon_service_artifacts(data_root)
    }

    fn cancel_core_finalization_generation_lease(
        &self,
        data_root: &Path,
        reason: &str,
    ) -> Result<()> {
        super::cancel_core_finalization_generation_lease(data_root, reason).map(|_| ())
    }

    fn observe_source_refresh_endpoint(
        &self,
        identity_path: &Path,
    ) -> ctx_daemon_application::DaemonEndpointObservation {
        let identity = std::fs::read_to_string(identity_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        ctx_daemon_application::DaemonEndpointObservation {
            available: identity.is_some(),
            transport: identity
                .as_ref()
                .and_then(|value| json_string(value, "transport")),
            owner_pid: identity.as_ref().and_then(|value| json_u32(value, "pid")),
            address: identity.as_ref().and_then(|value| {
                json_string(value, "path").or_else(|| json_string(value, "pipe_name"))
            }),
        }
    }

    fn deliver_daemon_events(&self, data_root: &Path, events: &[PublicEventV1]) {
        super::daemon_service_ports::OBSERVATION.deliver(data_root, events);
    }
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
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
    let host = CliDaemonApplicationHost::new();
    let application = ctx_daemon_application::DaemonApplication::new(&host);
    operation(&application)
}

pub(super) fn with_daemon_run_application<T>(
    config: &crate::config::AppConfig,
    operation: impl FnOnce(&ctx_daemon_application::DaemonApplication<'_>) -> T,
) -> T {
    let host = CliDaemonApplicationHost::for_daemon_run(config);
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

pub(super) fn resume_daemon_supervisor_after_upgrade(
    data_root: &Path,
    executable: &Path,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    with_daemon_application(|application| {
        application.resume_daemon_supervisor_after_upgrade(data_root, executable, upgrade_fence)
    })
}
