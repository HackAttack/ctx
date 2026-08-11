use std::process;

use ctx_daemon_runtime::DaemonLock;
use ctx_daemon_service::{
    DaemonRunArgs as ServiceDaemonRunArgs, DaemonStartMode, DaemonSupervisor, DaemonTrigger,
    DaemonUpgradePorts,
};

use super::super::{
    daemon_service_ports::{self, PORTS},
    paths_status::daemon_report,
};
use super::*;
use crate::DaemonTriggerCommandArg;

#[test]
fn post_lock_initialization_failure_retains_restart_intent() -> Result<()> {
    struct RestoreUpgradeTarget(Option<std::ffi::OsString>);

    impl Drop for RestoreUpgradeTarget {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("CTX_UPGRADE_TEST_TARGET", value),
                None => std::env::remove_var("CTX_UPGRADE_TEST_TARGET"),
            }
        }
    }

    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore_upgrade_target = RestoreUpgradeTarget(std::env::var_os("CTX_UPGRADE_TEST_TARGET"));
    let installation = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(installation.path(), fs::Permissions::from_mode(0o700))?;
    }
    let installation_executable =
        installation
            .path()
            .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&installation_executable, b"test ctx executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&installation_executable, fs::Permissions::from_mode(0o700))?;
    }
    std::env::set_var("CTX_UPGRADE_TEST_TARGET", &installation_executable);

    let root = tempfile::tempdir()?;
    super::super::daemon_autostart::write_daemon_restart_request(
        root.path(),
        DaemonTriggerCommandArg::Search,
        "ua_01890f3e-2c80-7000-8000-00000000000b",
    )?;
    fs::write(
        root.path().join(".fail-daemon-before-ready-for-test"),
        b"fail",
    )?;

    let engine = crate::upgrade::ports::engine();
    let upgrade = DaemonUpgradePorts {
        engine: &engine,
        daemon: &crate::upgrade::ports::DAEMON_UPGRADE,
        automatic_policy: &crate::upgrade::ports::AUTOMATIC_POLICY,
        observer: &crate::upgrade::ports::UPGRADE_OBSERVER,
    };
    let error = ctx_daemon_service::run_daemon(
        ServiceDaemonRunArgs {
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartMode::Auto),
            trigger_command: Some(DaemonTrigger::Search),
            supervisor: DaemonSupervisor::CliAutostart,
        },
        root.path(),
        daemon_service_ports::config_snapshot(&AppConfig::default()),
        &PORTS,
        &upgrade,
    )
    .expect_err("the injected post-lock initialization failure must surface");

    let rendered_error = error.to_string();
    assert!(
        rendered_error.contains("injected daemon failure before readiness"),
        "unexpected daemon initialization error: {rendered_error}"
    );
    assert!(super::super::daemon_autostart::read_daemon_restart_request(root.path()).is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_refresh_only_status_exposes_runtime_and_certified_refresh_identity() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = ctx_history_core::utc_now().timestamp_millis();
    ctx_daemon_service::testing::write_daemon_lifecycle_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": process::id(),
            "started_at_ms": now,
            "heartbeat_at_ms": now,
            "start_mode": "auto",
            "trigger_command": "search",
            "semantic_runtime_active": false,
            "config_reload": {
                "status": "applied",
                "requested": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
            },
        }),
    )?;
    ctx_daemon_service::testing::write_daemon_service_endpoint(
        temp.path(),
        DaemonIpcService::SourceRefresh,
        &DaemonQueryEndpoint::Unix {
            path: temp.path().join("daemon/source-refresh.sock"),
            token: "must-not-appear-in-status-00000000".to_owned(),
        },
    )?;
    ctx_daemon_service::testing::write_core_refresh_status(
        temp.path(),
        &json!({
            "status": "completed",
            "daemon_mode": "source-refresh-only",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "certified_source_count": 4,
            "certified_source_bytes": 8192,
            "timings_us": {
                "discovery": 5,
                "scan_stage": 7,
                "commit": 11,
            },
        }),
    )?;

    let report = daemon_report(temp.path());

    assert_eq!(report["mode"], "source-refresh-only");
    assert_eq!(report["live_pid"], process::id());
    assert_eq!(report["trigger_command"], "search");
    assert_eq!(report["trigger_provenance"], "autostart");
    assert_eq!(report["lock_identity"]["active"], true);
    assert!(report["lock_identity"]["owner_id"]
        .as_str()
        .is_some_and(|owner| !owner.is_empty()));
    assert_eq!(report["core_refresh_endpoint"]["available"], true);
    assert_eq!(report["core_refresh_endpoint"]["owner_pid"], process::id());
    assert!(!report.to_string().contains("must-not-appear-in-status"));
    assert_eq!(
        report["jobs"]["semantic_index"]["reason"],
        "daemon_mode_source_refresh_only"
    );
    assert_eq!(report["jobs"]["core_refresh"]["certified_source_count"], 4);
    assert_eq!(
        report["jobs"]["core_refresh"]["certified_source_bytes"],
        8192
    );
    for stage in ["discovery", "scan_stage", "commit"] {
        assert!(
            report["jobs"]["core_refresh"]["timings_us"][stage]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "{stage}"
        );
    }
    drop(lock);
    Ok(())
}
