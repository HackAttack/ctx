use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{anyhow, Result};
use ctx_terminal::{print_json, Ui};
use serde_json::json;

use crate::{
    config::{AppConfig, CONFIG_FILE},
    DaemonArgs, DaemonCommand, DaemonDisableArgs, DaemonRunArgs, DaemonStartModeArg, FormatArgs,
};

use super::{
    daemon_status::{
        daemon_report_failure_message, render_daemon_disable_receipt, render_daemon_enable_receipt,
        render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
    },
    paths_status::daemon_report_with_disabled_status,
};

pub(crate) mod control;
#[cfg(test)]
mod seam_tests;
pub use control::run_daemon_command;

fn run_daemon(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    args: DaemonRunArgs,
    data_root: PathBuf,
    ui: &mut Ui,
) -> Result<()> {
    let request = ctx_daemon_application::DaemonHostRunRequest {
        idle_exit_seconds: args.idle_exit_seconds,
        loop_interval_seconds: args.loop_interval_seconds,
        max_chunks: args.max_chunks,
        force: args.force,
        start_mode: args.start_mode.map(|mode| match mode {
            DaemonStartModeArg::Manual => ctx_daemon_application::DaemonHostStartMode::Manual,
            DaemonStartModeArg::Auto => ctx_daemon_application::DaemonHostStartMode::Auto,
        }),
        trigger: args
            .trigger_command
            .map(super::daemon_supervisor::daemon_trigger),
    };
    application.run_daemon_host(&data_root, request).map_err(|error| match error {
        ctx_daemon_application::DaemonHostRunError::InternalAutostartMetadata => anyhow!(
            "daemon autostart metadata flags are internal; run `ctx daemon run` without --start-mode or --trigger-command"
        ),
        ctx_daemon_application::DaemonHostRunError::Service(error) => error,
    })?;

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
            return Err(crate::RenderedCliError.into());
        }
        return Err(anyhow!(message));
    }
    Ok(())
}
