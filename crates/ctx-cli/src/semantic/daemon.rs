use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_client_observability::analytics::{
    DaemonOperationV1, OperationCompletedV1, Outcome, PublicEventV1,
};
use ctx_daemon_service::{
    DaemonObservationPort, DaemonRunArgs as ServiceDaemonRunArgs, DaemonStartMode,
    DaemonSupervisor, DaemonTrigger, DaemonUpgradePorts,
};
use serde_json::{json, Value};

use crate::{
    compact_json,
    config::{self, AppConfig, CONFIG_FILE},
    output::print_json,
    DaemonArgs, DaemonCommand, DaemonDisableArgs, DaemonRunArgs, DaemonStartModeArg,
    DaemonTriggerCommandArg, FormatArgs,
};

use super::{
    daemon_autostart::terminate_current_executable_daemon,
    daemon_service_ports::{self, OBSERVATION, PORTS},
    daemon_status::{
        daemon_report_failure_message, render_daemon_disable_receipt, render_daemon_enable_receipt,
        render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
    },
    health_search::semantic_env_flag,
    paths_status::{daemon_lock_is_active, daemon_report, daemon_report_with_disabled_status},
    query_service::{
        daemon_service_endpoint_path, daemon_source_refresh_request,
        read_daemon_service_endpoint_identity, DaemonIpcService,
    },
    runtime_limits::DAEMON_BACKGROUND_CHILD_ENV,
};
use crate::ui::Ui;

#[cfg(unix)]
use super::query_service::DaemonQueryEndpoint;

mod control;
#[cfg(test)]
mod seam_tests;
pub(crate) use control::run_daemon_command;

fn run_daemon(
    args: DaemonRunArgs,
    data_root: PathBuf,
    config: &AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    if (args.start_mode.is_some() || args.trigger_command.is_some())
        && !semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
    {
        return Err(anyhow!(
            "daemon autostart metadata flags are internal; run `ctx daemon run` without --start-mode or --trigger-command"
        ));
    }
    let service_args = ServiceDaemonRunArgs {
        idle_exit_seconds: args.idle_exit_seconds,
        loop_interval_seconds: args.loop_interval_seconds,
        max_chunks: args.max_chunks,
        max_seconds: None,
        force: args.force,
        start_mode: args.start_mode.map(|mode| match mode {
            DaemonStartModeArg::Manual => DaemonStartMode::Manual,
            DaemonStartModeArg::Auto => DaemonStartMode::Auto,
        }),
        trigger_command: args.trigger_command.map(|trigger| match trigger {
            DaemonTriggerCommandArg::Setup => DaemonTrigger::Setup,
            DaemonTriggerCommandArg::Import => DaemonTrigger::Import,
            DaemonTriggerCommandArg::Search => DaemonTrigger::Search,
        }),
        supervisor: if matches!(args.start_mode, Some(DaemonStartModeArg::Auto))
            && semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
        {
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
        &data_root,
        daemon_service_ports::config_snapshot(config),
        &PORTS,
        &upgrade,
    )?;

    let report = daemon_report_with_disabled_status(&data_root, !args.force);
    let failure = daemon_report_failure_message(&report);
    if args.format.is_json() {
        print_json(report)?;
    } else {
        let document =
            render_daemon_status_human(ui.stdout_context(), DaemonStatusView::daemon_only(&report));
        ui.write_stdout(&document)?;
    }
    if let Some(message) = failure {
        if !args.format.is_json() {
            return Err(crate::dispatch::rendered_cli_error());
        }
        return Err(anyhow!(message));
    }
    Ok(())
}

fn send_daemon_events(data_root: &std::path::Path, events: &[PublicEventV1]) {
    OBSERVATION.deliver(data_root, events);
}
