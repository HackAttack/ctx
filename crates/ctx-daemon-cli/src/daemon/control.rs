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
            DaemonCommand::Lifecycle(args) => {
                run_daemon_lifecycle_update(application, args, data_root, ui)
            }
            DaemonCommand::Enable(args) => run_daemon_lifecycle_update(
                application,
                DaemonLifecycleArgs {
                    lifecycle: crate::DaemonLifecycle::Persistent,
                    format: args.format,
                },
                data_root,
                ui,
            ),
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
        DaemonCommand::Lifecycle(args) => match args.lifecycle {
            crate::DaemonLifecycle::Persistent => {
                Some(ctx_daemon_application::DaemonObservedOperation::Enable)
            }
            crate::DaemonLifecycle::OnDemand => None,
            crate::DaemonLifecycle::Disabled => {
                Some(ctx_daemon_application::DaemonObservedOperation::Disable)
            }
        },
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

pub(super) fn run_daemon_lifecycle_update(
    application: &ctx_daemon_application::DaemonApplication<'_>,
    args: DaemonLifecycleArgs,
    data_root: PathBuf,
    ui: &mut Ui,
) -> Result<()> {
    let requested_lifecycle = args.lifecycle;
    let update = application
        .update_daemon_lifecycle(
            &data_root,
            match requested_lifecycle {
                crate::DaemonLifecycle::Persistent => {
                    ctx_daemon_application::DaemonLifecycle::Persistent
                }
                crate::DaemonLifecycle::OnDemand => {
                    ctx_daemon_application::DaemonLifecycle::OnDemand
                }
                crate::DaemonLifecycle::Disabled => {
                    ctx_daemon_application::DaemonLifecycle::Disabled
                }
            },
        )
        .map_err(daemon_lifecycle_update_error)?;
    let lifecycle = match update.lifecycle {
        ctx_daemon_application::DaemonLifecycle::Persistent => crate::DaemonLifecycle::Persistent,
        ctx_daemon_application::DaemonLifecycle::OnDemand => crate::DaemonLifecycle::OnDemand,
        ctx_daemon_application::DaemonLifecycle::Disabled => crate::DaemonLifecycle::Disabled,
    };
    let supervisor = update.supervisor.into_json();
    let persistent = update.persistent;
    let running = update.running;
    let pid = update.pid;
    let config_path = data_root.join(CONFIG_FILE);
    if args.format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemon_lifecycle": lifecycle.as_str(),
            "daemon_enabled": lifecycle.starts_implicitly(),
            "running": running,
            "pid": pid,
            "persistent": persistent,
            "supervisor": supervisor,
            "config_path": config_path,
            "local_only": true,
        }))?;
    } else if lifecycle.is_persistent() {
        let document = render_daemon_enable_receipt(
            ui.stdout_context(),
            running,
            persistent,
            &supervisor,
            &config_path,
        );
        ui.write_stdout(&document)?;
    } else if lifecycle.is_on_demand() {
        let document =
            render_daemon_on_demand_receipt(ui.stdout_context(), &supervisor, &config_path);
        ui.write_stdout(&document)?;
    } else {
        let document =
            render_daemon_disable_receipt(ui.stdout_context(), &supervisor, &config_path);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn daemon_lifecycle_update_error(
    error: ctx_daemon_application::DaemonLifecycleUpdateError,
) -> anyhow::Error {
    match error {
        ctx_daemon_application::DaemonLifecycleUpdateError::Operation(error) => error,
        ctx_daemon_application::DaemonLifecycleUpdateError::StartSuppressed => anyhow!(
            "ctx daemon start was suppressed (hosted_uninstall_active); retry after it clears or run `ctx setup --no-daemon`"
        ),
        ctx_daemon_application::DaemonLifecycleUpdateError::Supervisor(error) => {
            error.context("establish ctx daemon supervision")
        }
        ctx_daemon_application::DaemonLifecycleUpdateError::Start(error) => match error {
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
        return run_daemon_lifecycle_update(
            application,
            DaemonLifecycleArgs {
                lifecycle: crate::DaemonLifecycle::Disabled,
                format: args.format,
            },
            data_root,
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
