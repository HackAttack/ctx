use super::*;

pub fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig<'_>,
    ui: &mut Ui,
) -> Result<()> {
    super::super::daemon_supervisor::with_daemon_run_application(config, |application| {
        let started = Instant::now();
        let operation = daemon_operation_for_command(&args.command);
        let telemetry_root = data_root.clone();
        let result = match args.command {
            DaemonCommand::Run(args) => run_daemon(application, args, data_root, ui),
            DaemonCommand::Status(args) => run_daemon_status(application, args, data_root, ui),
            DaemonCommand::Enable(args) => {
                run_daemon_enabled_update(application, args, data_root, true, ui)
            }
            DaemonCommand::Disable(args) => run_daemon_disable(application, args, data_root, ui),
        };
        if let Some(operation) = operation {
            application.observe_daemon_operation(
                &telemetry_root,
                operation,
                result.is_ok(),
                started.elapsed(),
            );
        }
        result
    })
}

fn daemon_operation_for_command(
    command: &DaemonCommand,
) -> Option<ctx_daemon_application::DaemonObservedOperation> {
    match command {
        DaemonCommand::Run(_) => None,
        DaemonCommand::Status(_) => Some(ctx_daemon_application::DaemonObservedOperation::Status),
        DaemonCommand::Enable(_) => Some(ctx_daemon_application::DaemonObservedOperation::Enable),
        DaemonCommand::Disable(_) => Some(ctx_daemon_application::DaemonObservedOperation::Disable),
    }
}

pub(super) fn run_daemon_status(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    args: FormatArgs,
    data_root: PathBuf,
    ui: &mut Ui,
) -> Result<()> {
    let daemon =
        super::super::paths_status::daemon_report_with_application(application, &data_root, true);
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon": daemon,
            "local_only": true,
        }))?;
    } else {
        let document =
            render_daemon_status_human(ui.stdout_context(), DaemonStatusView::daemon_only(&daemon));
        ui.write_stdout(&document)?;
    }
    Ok(())
}

pub(super) fn run_daemon_enabled_update(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    args: FormatArgs,
    data_root: PathBuf,
    enabled: bool,
    ui: &mut Ui,
) -> Result<()> {
    let update = application
        .update_daemon_enabled(&data_root, enabled)
        .map_err(daemon_enabled_update_error)?;
    let supervisor = update.supervisor.into_json();
    let persistent = update.persistent;
    let running = update.running;
    let pid = update.pid;
    let config_path = data_root.join(CONFIG_FILE);
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "running": running,
            "pid": pid,
            "persistent": persistent,
            "supervisor": supervisor,
            "config_path": config_path,
            "local_only": true,
        }))?;
    } else if enabled {
        let document = render_daemon_enable_receipt(
            ui.stdout_context(),
            running,
            persistent,
            &supervisor,
            &config_path,
        );
        ui.write_stdout(&document)?;
    } else {
        let document =
            render_daemon_disable_receipt(ui.stdout_context(), &supervisor, &config_path);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn daemon_enabled_update_error(
    error: ctx_daemon_application::DaemonEnabledUpdateError,
) -> anyhow::Error {
    match error {
        ctx_daemon_application::DaemonEnabledUpdateError::Operation(error) => error,
        ctx_daemon_application::DaemonEnabledUpdateError::StartSuppressed => anyhow!(
            "ctx daemon start was suppressed (hosted_uninstall_active); retry after it clears or run `ctx setup --no-daemon`"
        ),
        ctx_daemon_application::DaemonEnabledUpdateError::Supervisor(error) => {
            error.context("establish ctx daemon supervision")
        }
        ctx_daemon_application::DaemonEnabledUpdateError::Start(error) => match error {
            ctx_daemon_application::DaemonStartError::Suppressed(reason) => anyhow!(
                "ctx daemon start was suppressed ({reason}); retry after it clears or run `ctx setup --no-daemon`"
            ),
            ctx_daemon_application::DaemonStartError::BinaryIdentity(error) => error,
            ctx_daemon_application::DaemonStartError::Start(error) => anyhow!(
                "ctx daemon did not start: {error:#}. Run `ctx daemon status --format json`, then `ctx daemon run` for details"
            ),
            ctx_daemon_application::DaemonStartError::Ready(error) => anyhow!(
                "ctx daemon did not become ready: {error}. Run `ctx daemon status --format json`, then `ctx daemon run` for details"
            ),
        },
    }
}

fn run_daemon_disable(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    args: DaemonDisableArgs,
    data_root: PathBuf,
    ui: &mut Ui,
) -> Result<()> {
    if !args.prepare_uninstall {
        return run_daemon_enabled_update(
            application,
            FormatArgs {
                format: args.format,
            },
            data_root,
            false,
            ui,
        );
    }
    let report = super::super::daemon_autostart::prepare_daemon_uninstall(&data_root)?;
    if args.format.is_json() {
        print_json(report)?;
    } else {
        let document = render_daemon_prepare_uninstall_receipt(ui.stdout_context(), &report);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

pub(crate) fn remove_released_daemon_service_artifacts(data_root: &Path) -> Result<()> {
    let endpoint_paths = [
        ctx_daemon_service::DaemonIpcService::SemanticQuery,
        ctx_daemon_service::DaemonIpcService::SourceRefresh,
    ]
    .map(|service| ctx_daemon_service::daemon_service_endpoint_path(data_root, service));
    ctx_daemon_runtime::remove_released_daemon_service_endpoints(data_root, &endpoint_paths)
}
