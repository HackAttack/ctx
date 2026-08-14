use std::path::PathBuf;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::analytics::{self, SetupMode, SetupTelemetry};
use crate::history_config::CliHistoryConfigAdapter;
use crate::output::print_json;
use crate::progress::{ProgressReporter, ProgressWriterError};
use crate::semantic::{
    autostart_daemon_for_setup_and_wait, coordinate_source_backed_refresh_with_progress,
    daemon_autostart_suppression_reason, semantic_query_service_supported,
    source_epoch_status_report, DaemonSetupHandoff, SourceBackedRefreshMode,
    SourceBackedRefreshPendingPublication,
};
use crate::ui::Ui;
use crate::{config, SetupArgs};
use ctx_cli_presentation::commands::{render_setup_human, SetupDaemonState};
use ctx_history_cli::HistoryConfigPort;

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    telemetry: &mut SetupTelemetry,
    _provider_refreshes: &mut crate::commands::import::ProviderRefreshCollector,
    quiet: bool,
    config: &mut config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    let semantic_supported = semantic_query_service_supported();
    if args.semantic && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "`ctx setup --semantic` requires daemon maintenance. Enable [daemon] enabled = true and rerun without --no-daemon"
        );
    }
    if args.semantic {
        CliHistoryConfigAdapter::new(&data_root, config).set_semantic_search_enabled(true)?;
    }
    let semantic_enabled = config.semantic_search_enabled();
    if semantic_enabled && semantic_supported && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, remove --no-daemon, or set [search] semantic = false"
        );
    }

    CliHistoryConfigAdapter::new(&data_root, config).write_default_config()?;

    let json_output = args.format.is_json();
    let suppression_reason = daemon_autostart_suppression_reason();
    let daemon_autostart_requested =
        config.daemon.enabled && !args.no_daemon && suppression_reason.is_none();
    let daemon_autostart_reason = if args.no_daemon {
        Some("explicit_opt_out")
    } else if !config.daemon.enabled {
        Some("daemon_disabled")
    } else {
        suppression_reason
    };
    let daemon_handoff = if daemon_autostart_requested {
        Some(autostart_daemon_for_setup_and_wait(
            &data_root,
            config,
            crate::DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        None
    };
    let refresh_wait = args.wait;
    let refresh_request = {
        let mut progress = ProgressReporter::new(ui, args.progress.into(), json_output, "setup", 0);
        request_source_refresh(
            &data_root,
            config.daemon.enabled,
            args.no_daemon,
            refresh_wait,
            daemon_autostart_reason,
            &mut progress,
        )?
    };
    let source = source_epoch_status_report(&data_root, config)?;
    let supervisor = source.report["daemon"]["supervisor"].clone();
    let lexical_status = source.report["lexical"]["status"]
        .as_str()
        .unwrap_or("unavailable");
    let refresh_health_status = source.report["refresh"]["status"]
        .as_str()
        .unwrap_or("unavailable");
    let mode = setup_mode(
        lexical_status,
        refresh_request["status"].as_str().unwrap_or("unavailable"),
        refresh_health_status,
    );
    telemetry.mode = Some(if mode == "ready" {
        SetupMode::Ready
    } else {
        SetupMode::Background
    });
    telemetry.providers_detected = source.indexed_sources.map(analytics::count_bucket);
    telemetry.has_indexed_content = source.indexed_items.map(|count| count > 0);

    let mut output = source.report.clone();
    let Some(output_fields) = output.as_object_mut() else {
        bail!("source status report was not an object");
    };
    output_fields.remove("read_only");
    output_fields.insert("mode".to_owned(), json!(mode));
    output_fields.insert("refresh_request".to_owned(), refresh_request.clone());
    output_fields.insert(
        "daemon_autostart".to_owned(),
        daemon_autostart_json(
            daemon_autostart_requested,
            daemon_autostart_reason,
            daemon_handoff.as_ref(),
            &supervisor,
        ),
    );
    output_fields.insert(
        "deprecated_catalog_only_ignored".to_owned(),
        json!(args.catalog_only),
    );
    output_fields.insert("network_required".to_owned(), json!(false));
    output_fields.insert("repo_writes".to_owned(), json!(false));

    if json_output {
        print_json(output)?;
    } else if !quiet {
        let document = render_setup_human(
            ui.stdout_context(),
            &data_root,
            mode,
            &source.report,
            &refresh_request,
            SetupDaemonState {
                requested: daemon_autostart_requested,
                reason: daemon_autostart_reason,
                started: daemon_handoff.is_some(),
                persistent_supervisor_verified: supervisor_persistently_verified(&supervisor),
            },
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn setup_mode(
    lexical_status: &str,
    refresh_status: &str,
    refresh_health_status: &str,
) -> &'static str {
    match lexical_status {
        "ready" if refresh_health_status == "ready" => "ready",
        "pending" => "pending",
        "stale" => "stale",
        _ if refresh_status == "pending" => "pending",
        _ => "unavailable",
    }
}

fn request_source_refresh(
    data_root: &std::path::Path,
    daemon_enabled: bool,
    no_daemon: bool,
    wait: bool,
    daemon_unavailable_reason: Option<&str>,
    progress: &mut ProgressReporter<'_>,
) -> Result<Value> {
    if no_daemon || !daemon_enabled {
        return Ok(json!({
            "status": "unavailable",
            "reason": if no_daemon {
                "explicit_opt_out"
            } else {
                "daemon_disabled"
            },
            "mode": if wait { "wait" } else { "background" },
            "daemon_available": false,
        }));
    }
    let mode = if wait {
        SourceBackedRefreshMode::Wait
    } else {
        SourceBackedRefreshMode::Background
    };
    let mut report_progress = |status: &crate::semantic::RefreshStatus| {
        progress.source_refresh(status).map_err(anyhow::Error::new)
    };
    let mut effective_wait = wait;
    let mut result =
        coordinate_source_backed_refresh_with_progress(data_root, mode, &mut report_progress);
    if result.as_ref().is_err_and(|error| {
        !wait
            && error
                .downcast_ref::<SourceBackedRefreshPendingPublication>()
                .is_some_and(|pending| pending.source_count() == 0)
    }) {
        // A fresh empty catalog has no meaningful background work to defer.
        // Reattach through the same daemon/RefreshEngine and wait for its
        // verified empty publication instead of returning an uncertified Core.
        effective_wait = true;
        result = coordinate_source_backed_refresh_with_progress(
            data_root,
            SourceBackedRefreshMode::Wait,
            &mut report_progress,
        );
    }
    match result {
        Ok(observation) => {
            let receipt = observation
                .receipt
                .as_ref()
                .map(|receipt| receipt.to_json());
            Ok(json!({
                "status": observation.status,
                "reason": Value::Null,
                "mode": if effective_wait { "wait" } else { "background" },
                "request_id": observation.request_id,
                "daemon_available": observation.daemon_available,
                "source_count": observation.source_count,
                "published_generation": observation.pin.generation_id(),
                "receipt": receipt,
            }))
        }
        Err(error) => {
            if error
                .chain()
                .any(|cause| cause.downcast_ref::<ProgressWriterError>().is_some())
            {
                return Err(error);
            }
            Ok(refresh_request_failure(
                &error,
                effective_wait,
                daemon_unavailable_reason,
            ))
        }
    }
}

fn refresh_request_failure(
    error: &anyhow::Error,
    wait: bool,
    daemon_unavailable_reason: Option<&str>,
) -> Value {
    let daemon_unavailable = error
        .downcast_ref::<crate::semantic::SourceBackedRefreshDaemonUnavailable>()
        .is_some();
    let pending = (!wait)
        .then(|| error.downcast_ref::<SourceBackedRefreshPendingPublication>())
        .flatten();
    json!({
        "status": if pending.is_some() { "pending" } else { "unavailable" },
        "reason": if daemon_unavailable {
            daemon_unavailable_reason.unwrap_or("daemon_unavailable")
        } else if pending.is_some() {
            "refresh_queued_without_published_generation"
        } else {
            "refresh_failed"
        },
        "mode": if wait { "wait" } else { "background" },
        "request_id": pending.map(SourceBackedRefreshPendingPublication::request_id),
        "request_state": pending.map(SourceBackedRefreshPendingPublication::request_state),
        "source_count": pending.map(SourceBackedRefreshPendingPublication::source_count),
        "daemon_available": !daemon_unavailable,
        "last_error": format!("{error:#}"),
    })
}

fn daemon_autostart_json(
    requested: bool,
    reason: Option<&str>,
    startup: Option<&DaemonSetupHandoff>,
    supervisor: &Value,
) -> Value {
    let persistently_supervised = supervisor_persistently_verified(supervisor);
    match startup {
        Some(startup) => {
            json!({
                "status": if persistently_supervised { "verified" } else { "degraded" },
                "reason": if persistently_supervised {
                    Value::Null
                } else {
                    Value::String("native_supervisor_unavailable".to_owned())
                },
                "requested": requested,
                "pid": startup.handoff.pid,
                "persistent": true,
                "limitation": Value::Null,
                "supervisor": supervisor,
                "status_command": "ctx daemon status",
            })
        }
        None => json!({
            "status": if requested { "unavailable" } else { "not_requested" },
            "reason": reason.unwrap_or("not_requested"),
            "requested": requested,
            "persistent": false,
            "limitation": Value::Null,
            "supervisor": supervisor,
            "status_command": "ctx daemon status",
        }),
    }
}

fn supervisor_persistently_verified(supervisor: &Value) -> bool {
    supervisor.get("status").and_then(Value::as_str) == Some("installed")
        && supervisor
            .get("registration_verified")
            .and_then(Value::as_bool)
            == Some(true)
        && supervisor
            .get("live_owner_verified")
            .and_then(Value::as_bool)
            == Some(true)
}

#[cfg(test)]
mod tests {
    use crate::semantic::DaemonHandoff;
    use serde_json::json;

    use super::*;

    #[test]
    fn admitted_background_refresh_reports_pending_before_first_publication() {
        assert_eq!(setup_mode("unavailable", "pending", "pending"), "pending");
        assert_eq!(
            setup_mode("unavailable", "unavailable", "unavailable"),
            "unavailable"
        );
        assert_eq!(setup_mode("stale", "pending", "stale"), "stale");
        assert_eq!(setup_mode("ready", "pending", "ready"), "ready");
        assert_eq!(setup_mode("ready", "published", "partial"), "unavailable");
        assert_eq!(
            setup_mode("ready", "published", "unavailable"),
            "unavailable"
        );
        assert_eq!(setup_mode("ready", "published", "stale"), "unavailable");
    }

    #[test]
    fn only_admitted_background_work_reports_pending() {
        let admitted: anyhow::Error = SourceBackedRefreshPendingPublication::new(
            "request-1".to_owned(),
            "admission_pending".to_owned(),
            7,
        )
        .into();
        let pending = refresh_request_failure(&admitted, false, None);
        assert_eq!(pending["status"], "pending");
        assert_eq!(
            pending["reason"],
            "refresh_queued_without_published_generation"
        );
        assert_eq!(pending["request_id"], "request-1");
        assert_eq!(pending["request_state"], "admission_pending");
        assert_eq!(pending["source_count"], 7);

        let rejected = refresh_request_failure(&anyhow::anyhow!("queue full"), false, None);
        assert_eq!(rejected["status"], "unavailable");
        assert_eq!(rejected["reason"], "refresh_failed");
        assert!(rejected["request_id"].is_null());

        let empty: anyhow::Error = SourceBackedRefreshPendingPublication::new(
            "empty-request".to_owned(),
            "queued".to_owned(),
            0,
        )
        .into();
        assert!(empty
            .downcast_ref::<SourceBackedRefreshPendingPublication>()
            .is_some_and(|pending| pending.source_count() == 0));
    }

    #[test]
    fn setup_manager_unavailable_reports_a_persistent_process_and_restart_limitation() {
        let supervisor = json!({
            "kind": "systemd_user",
            "status": "manager_unavailable",
            "registration_verified": false,
            "live_owner_verified": false,
            "limitation": "native automatic restart at login or reboot is unavailable because the systemd user manager is not operational",
        });
        let startup = DaemonSetupHandoff {
            handoff: DaemonHandoff {
                pid: 42,
                heartbeat_at_ms: 1,
            },
        };
        let autostart = daemon_autostart_json(true, None, Some(&startup), &supervisor);
        assert_eq!(autostart["status"], "degraded");
        assert_eq!(autostart["persistent"], true);
        assert!(autostart["limitation"].is_null());
        assert_eq!(autostart["reason"], "native_supervisor_unavailable");
        assert!(autostart["supervisor"]["limitation"]
            .as_str()
            .is_some_and(|message| message.contains("automatic restart")));
    }

    #[test]
    fn setup_fallback_reports_a_persistent_daemon_without_a_bounded_limitation() {
        let supervisor = json!({
            "kind": "cli_self_heal",
            "status": "fallback",
            "registration_verified": false,
            "live_owner_verified": false,
            "limitation": "native per-user restart registration requires the hosted installer and the default data root",
        });
        let startup = DaemonSetupHandoff {
            handoff: DaemonHandoff {
                pid: 42,
                heartbeat_at_ms: 1,
            },
        };
        let autostart = daemon_autostart_json(true, None, Some(&startup), &supervisor);
        assert_eq!(autostart["status"], "degraded");
        assert_eq!(autostart["persistent"], true);
        assert!(autostart["limitation"].is_null());
    }

    #[test]
    fn setup_source_has_no_legacy_store_runtime_dependency() {
        let source = include_str!("setup.rs");
        let runtime = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "ctx_history_store::Store",
            "Store::open",
            "run_import_internal",
            "inventory_available_sources",
            "ctx import --all",
        ] {
            assert!(
                !runtime.contains(forbidden),
                "setup retained forbidden legacy dependency {forbidden}"
            );
        }
    }
}
