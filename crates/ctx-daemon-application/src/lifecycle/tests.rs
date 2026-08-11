use super::*;
use std::cell::Cell;

const DAEMON_ENV_PROBE_STAGE: &str = "CTX_DAEMON_ENV_PROBE_STAGE";
const DAEMON_ENV_PROBE_EXPECTED_CHANNEL: &str = "CTX_DAEMON_ENV_PROBE_EXPECTED_CHANNEL";
const DAEMON_ENV_PRO_CHANNEL: &str = "CTX_PRO_CHANNEL";
const DAEMON_ENV_PROBE_TEST: &str =
    "lifecycle::tests::daemon_child_environment_preserves_supported_pro_channel_and_strips_authority";
const DAEMON_ENV_HOSTILE: &str = "CTX_UNTRUSTED_DAEMON_AMBIENT_SECRET";
const DAEMON_ENV_ALLOWED_SENTINEL: &str = "/ctx-daemon-allowed-home";

#[test]
fn daemon_child_environment_preserves_supported_pro_channel_and_strips_authority() -> Result<()> {
    match env::var(DAEMON_ENV_PROBE_STAGE).as_deref() {
        Ok("final") => {
            let expected_channel = env::var(DAEMON_ENV_PROBE_EXPECTED_CHANNEL)?;
            assert_eq!(env::var("HOME").as_deref(), Ok(DAEMON_ENV_ALLOWED_SENTINEL));
            if expected_channel == "default" {
                assert!(env::var_os(DAEMON_ENV_PRO_CHANNEL).is_none());
            } else {
                assert_eq!(
                    env::var(DAEMON_ENV_PRO_CHANNEL).as_deref(),
                    Ok(expected_channel.as_str())
                );
            }
            assert!(env::var_os(DAEMON_ENV_HOSTILE).is_none());
            assert!(env::var_os("CTX_RELEASE_INHERITED_AUTHORITY").is_none());
            assert!(env::var_os("CTX_RELEASE_CONFIGURED_AUTHORITY").is_none());
            assert!(env::var_os("CTX_PRO_STAGING_ACCESS_CLIENT_SECRET").is_none());
            assert!(env::var_os("CTX_PRO_QUALIFICATION_HELPER_PATH").is_none());
            assert!(env::var_os("CTX_PRO_API_URL").is_none());
            return Ok(());
        }
        Ok("inherited") => {
            let expected_channel = env::var(DAEMON_ENV_PROBE_EXPECTED_CHANNEL)?;
            assert_eq!(env::var(DAEMON_ENV_HOSTILE).as_deref(), Ok("attacker"));
            assert_eq!(
                env::var("CTX_RELEASE_INHERITED_AUTHORITY").as_deref(),
                Ok("attacker")
            );
            let args: Vec<OsString> = ["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"]
                .into_iter()
                .map(OsString::from)
                .collect();
            let overrides = BTreeMap::from([
                (
                    OsString::from(DAEMON_ENV_PROBE_STAGE),
                    OsString::from("final"),
                ),
                (
                    OsString::from(DAEMON_ENV_PROBE_EXPECTED_CHANNEL),
                    OsString::from(&expected_channel),
                ),
            ]);
            let mut forbidden = overrides.clone();
            forbidden.insert(
                OsString::from("CTX_RELEASE_CONFIGURED_AUTHORITY"),
                OsString::from("attacker"),
            );
            let forbidden_error =
                normalized_daemon_launch_for_test(env::current_exe()?, args.clone(), forbidden)
                    .expect_err("release authority must be rejected during normalization");
            assert_eq!(forbidden_error.kind(), io::ErrorKind::InvalidInput);
            let descendant =
                normalized_daemon_launch_for_test(env::current_exe()?, args, overrides);
            if expected_channel == "invalid" {
                let error =
                    descendant.expect_err("unsupported Pro channel must fail during normalization");
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert!(error.to_string().contains("must be stable or staging"));
            } else {
                assert!(spawn_detached_daemon_child(descendant?)?.wait()?.success());
            }
            return Ok(());
        }
        _ => {}
    }

    for (expected_channel, channel) in [
        ("default", None),
        ("stable", Some("stable")),
        ("staging", Some("staging")),
        ("invalid", Some("preview")),
    ] {
        let mut inherited = std::process::Command::new(env::current_exe()?);
        inherited
            .args(["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"])
            .env(DAEMON_ENV_PROBE_STAGE, "inherited")
            .env(DAEMON_ENV_PROBE_EXPECTED_CHANNEL, expected_channel)
            .env(DAEMON_ENV_HOSTILE, "attacker")
            .env("CTX_RELEASE_INHERITED_AUTHORITY", "attacker")
            .env("CTX_PRO_STAGING_ACCESS_CLIENT_SECRET", "attacker")
            .env("CTX_PRO_QUALIFICATION_HELPER_PATH", "/attacker/helper")
            .env("CTX_PRO_API_URL", "https://attacker.invalid")
            .env("HOME", DAEMON_ENV_ALLOWED_SENTINEL)
            .env_remove(DAEMON_ENV_PRO_CHANNEL);
        if let Some(channel) = channel {
            inherited.env(DAEMON_ENV_PRO_CHANNEL, channel);
        }
        assert!(inherited.status()?.success());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn autostart_child_detaches_from_the_invoking_terminal_session() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("record-session.sh");
    let receipt = temp.path().join("session.txt");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s ' \"$$\" >\"$CTX_DAEMON_TEST_RECEIPT\"\nps -o sid= -p \"$$\" >>\"$CTX_DAEMON_TEST_RECEIPT\"\nexec sleep 30\n",
    )?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions)?;

    let launch = normalized_daemon_launch_for_test(
        executable.clone(),
        Vec::new(),
        BTreeMap::from([(
            OsString::from("CTX_DAEMON_TEST_RECEIPT"),
            receipt.as_os_str().to_os_string(),
        )]),
    )?;
    let mut child = spawn_detached_daemon_child(launch)?;
    for _ in 0..100 {
        if fs::read_to_string(&receipt)
            .is_ok_and(|recorded| recorded.split_whitespace().count() == 2)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let recorded = fs::read_to_string(&receipt);
    child.kill()?;
    child.wait()?;
    let recorded = recorded?;
    let values = recorded
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(values, vec![child.id(), child.id()]);
    Ok(())
}

fn test_config() -> DaemonConfigSnapshot {
    DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
    }
}

#[test]
fn setup_handoff_wait_accepts_authoritative_running_observation_without_sleep() -> Result<()> {
    let status = json!({
        "status": "running",
        "pid": 41,
        "heartbeat_at_ms": 1234,
        "config_reload": {"status": "applied"},
    });
    let mut observations = std::collections::VecDeque::from([
        DaemonHandoffObservation::Pending,
        daemon_handoff_observation_from(Some(&status), Some(41), true, Some(41), None, 1234),
    ]);
    let pauses = std::cell::Cell::new(0);

    let handoff = wait_for_daemon_handoff_with(
        3,
        || {
            observations
                .pop_front()
                .unwrap_or(DaemonHandoffObservation::Pending)
        },
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )?;

    assert_eq!(
        handoff,
        DaemonHandoff {
            pid: 41,
            heartbeat_at_ms: 1234,
        }
    );
    assert_eq!(pauses.get(), 1);
    Ok(())
}

#[test]
fn setup_handoff_accepts_a_slow_healthy_daemon_owned_first_build() {
    let expected = test_config();
    let status = json!({
        "status": "running",
        "pid": 41,
        "started_at_ms": 1_000,
        "heartbeat_at_ms": 1_050,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    let refresh_job = json!({
        "owner": "daemon",
        "kind": "core_refresh",
        "status": "running",
        "request_id": "cold-build",
        "request_state": "running",
        "last_run_at_ms": 1_025,
        "progress": {
            "phase": "refreshing",
            "completed_sources": 800,
            "total_sources": 5_781,
        },
    });
    let observation = daemon_handoff_observation_from(
        Some(&status),
        Some(41),
        true,
        Some(41),
        Some(&expected),
        1_050,
    );
    let active_refresh = daemon_owned_source_refresh_is_active(
        Some(&status),
        Some(&refresh_job),
        Some(41),
        None,
        None,
        1_050,
    );

    assert!(active_refresh);
    assert_eq!(
        complete_daemon_handoff_observation(
            observation,
            Some(&status),
            Some(41),
            true,
            &expected,
            false,
            active_refresh,
        ),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: 41,
            heartbeat_at_ms: 1_050,
        })
    );
}

#[test]
fn setup_handoff_accepts_fresh_same_owner_job_progress_with_a_stale_lifecycle_heartbeat() {
    let expected = test_config();
    let now_ms = 100_000;
    let stale_heartbeat_at_ms = now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1;
    let status = json!({
        "status": "running",
        "pid": 41,
        "started_at_ms": 1_000,
        "heartbeat_at_ms": stale_heartbeat_at_ms,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    let refresh_job = json!({
        "owner": "daemon",
        "kind": "core_refresh",
        "status": "running",
        "request_id": "cold-build",
        "request_state": "running",
        "last_run_at_ms": 2_000,
        "progress": {
            "phase": "refreshing",
            "completed_sources": 3,
            "total_sources": 4,
        },
        "last_error": null,
    });

    assert_eq!(
        daemon_handoff_observation_with_refresh_job_from(
            Some(&status),
            Some(&refresh_job),
            Some(41),
            true,
            Some(41),
            Some(&expected),
            Some(now_ms),
            now_ms,
        ),
        (
            DaemonHandoffObservation::Running(DaemonHandoff {
                pid: 41,
                heartbeat_at_ms: stale_heartbeat_at_ms,
            }),
            true,
        )
    );

    for invalid_job in [
        {
            let mut job = refresh_job.clone();
            job["owner"] = json!("cli");
            job
        },
        {
            let mut job = refresh_job.clone();
            job["request_id"] = json!("");
            job
        },
        {
            let mut job = refresh_job.clone();
            job["request_state"] = json!("failed");
            job
        },
        {
            let mut job = refresh_job.clone();
            job["last_error"] = json!("refresh failed");
            job
        },
    ] {
        assert_eq!(
            daemon_handoff_observation_with_refresh_job_from(
                Some(&status),
                Some(&invalid_job),
                Some(41),
                true,
                Some(41),
                Some(&expected),
                Some(now_ms),
                now_ms,
            ),
            (DaemonHandoffObservation::Pending, false),
            "invalid owner/request/error state must not gain handoff authority: {invalid_job}"
        );
    }
}

#[test]
fn setup_handoff_accepts_an_immediately_ready_endpoint() {
    let expected = test_config();
    let status = json!({
        "status": "running",
        "pid": 42,
        "started_at_ms": 2_000,
        "heartbeat_at_ms": 2_010,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    let observation = daemon_handoff_observation_from(
        Some(&status),
        Some(42),
        true,
        Some(42),
        Some(&expected),
        2_010,
    );

    assert_eq!(
        complete_daemon_handoff_observation(
            observation,
            Some(&status),
            Some(42),
            true,
            &expected,
            true,
            false,
        ),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: 42,
            heartbeat_at_ms: 2_010,
        })
    );
}

#[test]
fn setup_handoff_rejects_real_daemon_and_refresh_failures() {
    let expected = test_config();
    let failed_status = json!({
        "status": "failed",
        "pid": 43,
        "started_at_ms": 3_000,
        "heartbeat_at_ms": 3_010,
        "last_error": "cold build failed",
    });
    let failed_observation = daemon_handoff_observation_from(
        Some(&failed_status),
        Some(43),
        true,
        Some(43),
        Some(&expected),
        3_010,
    );
    assert_eq!(
        complete_daemon_handoff_observation(
            failed_observation,
            Some(&failed_status),
            Some(43),
            true,
            &expected,
            false,
            false,
        ),
        DaemonHandoffObservation::Failed("cold build failed".to_owned())
    );

    let running_status = json!({
        "status": "running",
        "pid": 44,
        "started_at_ms": 4_000,
        "heartbeat_at_ms": 4_010,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    let failed_refresh = json!({
        "owner": "daemon",
        "kind": "core_refresh",
        "status": "failed",
        "request_id": "failed-build",
        "request_state": "failed",
        "last_run_at_ms": 4_005,
        "progress": {"phase": "failed"},
        "last_error": "invalid generation",
    });
    assert!(!daemon_owned_source_refresh_is_active(
        Some(&running_status),
        Some(&failed_refresh),
        Some(44),
        None,
        None,
        4_010,
    ));
    assert_eq!(
        complete_daemon_handoff_observation(
            daemon_handoff_observation_from(
                Some(&running_status),
                Some(44),
                true,
                Some(44),
                Some(&expected),
                4_010,
            ),
            Some(&running_status),
            Some(44),
            true,
            &expected,
            false,
            false,
        ),
        DaemonHandoffObservation::Pending
    );
}

#[test]
fn enabled_daemon_handoff_is_bounded_to_five_seconds() {
    let pauses = DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS.saturating_sub(1);
    let maximum_wait = DAEMON_UPGRADE_POLL_INTERVAL
        .checked_mul(u32::try_from(pauses).unwrap())
        .unwrap();
    assert_eq!(maximum_wait, Duration::from_secs(5));
    assert_eq!(DAEMON_SETUP_HANDOFF_TIMEOUT, maximum_wait);
}

fn test_daemon_owner(owner_id: &str, pid: u32) -> DaemonOwnerIdentity {
    DaemonOwnerIdentity {
        owner_id: owner_id.to_owned(),
        pid,
        started_at_ms: 1_000,
        binary_sha256: "0123456789abcdef".to_owned(),
    }
}

#[test]
fn hung_listener_is_terminated_only_after_bounded_unusable_owner_proof() -> Result<()> {
    let owner = test_daemon_owner("hung-owner", 41);
    let current_checks = Cell::new(0);
    let active_checks = Cell::new(0);
    let endpoint_checks = Cell::new(0);
    let terminations = Cell::new(0);

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || {
            current_checks.set(current_checks.get() + 1);
            Ok(Some(owner.clone()))
        },
        || {
            active_checks.set(active_checks.get() + 1);
            false
        },
        || {
            endpoint_checks.set(endpoint_checks.get() + 1);
            Ok(false)
        },
        |owner_id| {
            assert_eq!(owner_id, "hung-owner");
            terminations.set(terminations.get() + 1);
            Ok(())
        },
    )?;

    assert!(terminated);
    assert_eq!(current_checks.get(), 2);
    assert_eq!(active_checks.get(), 2);
    assert_eq!(endpoint_checks.get(), 1);
    assert_eq!(terminations.get(), 1);
    Ok(())
}

#[test]
fn concurrent_recovery_never_terminates_a_replacement_owner() -> Result<()> {
    let unusable_owner = test_daemon_owner("unusable-owner", 41);
    let replacement_owner = test_daemon_owner("replacement-owner", 42);
    let current_checks = Cell::new(0);
    let terminations = Cell::new(0);

    let terminated = recover_unusable_daemon_owner_with(
        &unusable_owner,
        || {
            let check = current_checks.get();
            current_checks.set(check + 1);
            Ok(Some(if check == 0 {
                unusable_owner.clone()
            } else {
                replacement_owner.clone()
            }))
        },
        || false,
        || Ok(false),
        |_| {
            terminations.set(terminations.get() + 1);
            Ok(())
        },
    )?;

    assert!(!terminated);
    assert_eq!(current_checks.get(), 2);
    assert_eq!(terminations.get(), 0);
    Ok(())
}

fn running_refresh_status(heartbeat_at_ms: i64) -> (Value, Value) {
    (
        json!({
            "status": "running",
            "pid": 41,
            "started_at_ms": 1_000,
            "heartbeat_at_ms": heartbeat_at_ms,
        }),
        json!({
            "owner": "daemon",
            "kind": "core_refresh",
            "status": "running",
            "request_id": "slow-refresh",
            "request_state": "running",
            "last_run_at_ms": 2_000,
            "progress": {
                "phase": "refreshing",
                "completed_sources": 800,
                "total_sources": 5_781,
            },
        }),
    )
}

#[test]
fn stale_running_refresh_does_not_suppress_bounded_owner_takeover() -> Result<()> {
    let owner = test_daemon_owner("stale-refresh-owner", 41);
    let now_ms = 100_000;
    let stale_at_ms = now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1;
    let (status, refresh_job) = running_refresh_status(stale_at_ms);
    let endpoint_checks = Cell::new(0);
    let terminations = Cell::new(0);

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || Ok(Some(owner.clone())),
        || {
            daemon_owned_source_refresh_is_active(
                Some(&status),
                Some(&refresh_job),
                Some(owner.pid),
                Some(owner.started_at_ms),
                Some(stale_at_ms),
                now_ms,
            )
        },
        || {
            endpoint_checks.set(endpoint_checks.get() + 1);
            Ok(false)
        },
        |owner_id| {
            assert_eq!(owner_id, "stale-refresh-owner");
            terminations.set(terminations.get() + 1);
            Ok(())
        },
    )?;

    assert!(terminated);
    assert_eq!(endpoint_checks.get(), 1);
    assert_eq!(terminations.get(), 1);
    Ok(())
}

#[test]
fn fresh_slow_refresh_progress_prevents_unusable_endpoint_takeover() -> Result<()> {
    let owner = test_daemon_owner("refresh-owner", 41);
    let now_ms = 100_000;
    let stale_heartbeat_at_ms = now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1;
    let (status, refresh_job) = running_refresh_status(stale_heartbeat_at_ms);
    let endpoint_checks = Cell::new(0);
    let terminations = Cell::new(0);

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || Ok(Some(owner.clone())),
        || {
            daemon_owned_source_refresh_is_active(
                Some(&status),
                Some(&refresh_job),
                Some(owner.pid),
                Some(owner.started_at_ms),
                Some(now_ms),
                now_ms,
            )
        },
        || {
            endpoint_checks.set(endpoint_checks.get() + 1);
            Ok(false)
        },
        |_| {
            terminations.set(terminations.get() + 1);
            Ok(())
        },
    )?;

    assert!(!terminated);
    assert_eq!(endpoint_checks.get(), 0);
    assert_eq!(terminations.get(), 0);
    Ok(())
}

#[test]
fn setup_handoff_waits_for_requested_config_instead_of_previous_applied_mode() {
    let expected = test_config();
    let previous = json!({
        "status": "running",
        "pid": 41,
        "heartbeat_at_ms": 1234,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": "source-refresh-only",
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    assert_eq!(
        daemon_handoff_observation_from(
            Some(&previous),
            Some(41),
            true,
            None,
            Some(&expected),
            1234,
        ),
        DaemonHandoffObservation::Pending
    );

    let current = json!({
        "status": "running",
        "pid": 42,
        "heartbeat_at_ms": 1235,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });
    assert_eq!(
        daemon_handoff_observation_from(
            Some(&current),
            Some(42),
            true,
            None,
            Some(&expected),
            1235,
        ),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: 42,
            heartbeat_at_ms: 1235,
        })
    );
}

#[test]
fn setup_handoff_wait_surfaces_daemon_failure_without_sleep() {
    let status = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 1235,
        "last_error": "query service failed",
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        3,
        || daemon_handoff_observation_from(Some(&status), None, false, Some(42), None, 1235),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("failed status must reject the handoff");

    assert_eq!(error.to_string(), "query service failed");
    assert_eq!(pauses.get(), 0);
}

#[test]
fn setup_handoff_wait_ignores_stale_or_unowned_existing_failure_without_sleep() {
    let stale = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 1_000,
        "last_error": "old failure",
    });
    let unowned = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 35_000,
        "last_error": "unowned failure",
    });

    for (status, lock_pid, lock_active) in [
        (&stale, Some(42), true),
        (&unowned, Some(43), true),
        (&unowned, Some(42), false),
    ] {
        let pauses = std::cell::Cell::new(0);
        let error = wait_for_daemon_handoff_with(
            2,
            || {
                daemon_handoff_observation_from(
                    Some(status),
                    lock_pid,
                    lock_active,
                    None,
                    None,
                    35_000,
                )
            },
            || Ok(None),
            || pauses.set(pauses.get() + 1),
        )
        .expect_err("stale or unowned existing failure must remain pending");

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(pauses.get(), 1);
    }
}

#[test]
fn setup_handoff_wait_surfaces_fresh_owned_existing_failure_without_sleep() {
    let status = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 35_000,
        "last_error": "current failure",
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        2,
        || daemon_handoff_observation_from(Some(&status), Some(42), true, None, None, 35_000),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("fresh failure owned by the active daemon must reject the handoff");

    assert_eq!(error.to_string(), "current failure");
    assert_eq!(pauses.get(), 0);
}

#[test]
fn setup_handoff_wait_times_out_on_status_lock_identity_race_without_sleep() {
    let status = json!({
        "status": "running",
        "pid": 43,
        "heartbeat_at_ms": 1236,
        "config_reload": {"status": "applied"},
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        3,
        || daemon_handoff_observation_from(Some(&status), Some(44), true, Some(43), None, 1236),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("mismatched status and lock identities must not become ready");

    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_eq!(pauses.get(), 2);
}

#[test]
fn setup_handoff_wait_rejects_stale_or_future_heartbeat_without_sleep() {
    for heartbeat_at_ms in [1_000, 40_001] {
        let status = json!({
            "status": "running",
            "pid": 45,
            "heartbeat_at_ms": heartbeat_at_ms,
            "config_reload": {"status": "applied"},
        });
        let pauses = std::cell::Cell::new(0);

        let error = wait_for_daemon_handoff_with(
            2,
            || {
                daemon_handoff_observation_from(
                    Some(&status),
                    Some(45),
                    true,
                    Some(45),
                    None,
                    35_000,
                )
            },
            || Ok(None),
            || pauses.set(pauses.get() + 1),
        )
        .expect_err("an implausible heartbeat must not verify daemon readiness");

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(pauses.get(), 1);
    }
}

#[test]
fn responsive_owned_endpoint_allows_an_idle_event_driven_daemon() {
    let mut expected = test_config();
    expected.enabled = true;
    let status = json!({
        "status": "running",
        "pid": 45,
        "heartbeat_at_ms": 1_000,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": true,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });

    assert_eq!(
        daemon_live_endpoint_observation_from(Some(&status), Some(45), &expected),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: 45,
            heartbeat_at_ms: 1_000,
        })
    );
}

#[test]
fn responsive_endpoint_does_not_waive_owner_or_config_identity() {
    let mut expected = test_config();
    expected.enabled = true;
    let status = json!({
        "status": "running",
        "pid": 45,
        "heartbeat_at_ms": 1_000,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": false,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
            },
        },
    });

    assert_eq!(
        daemon_live_endpoint_observation_from(Some(&status), Some(46), &expected),
        DaemonHandoffObservation::Pending
    );
    assert_eq!(
        daemon_live_endpoint_observation_from(Some(&status), Some(45), &expected),
        DaemonHandoffObservation::Pending
    );
}

#[test]
fn setup_handoff_wait_ignores_stale_nested_config_failure_without_sleep() {
    let status = json!({
        "status": "running",
        "pid": 45,
        "heartbeat_at_ms": 1_000,
        "last_error": "old daemon failure",
        "config_reload": {
            "status": "activation_failed",
            "last_error": "old config failure",
        },
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        2,
        || daemon_handoff_observation_from(Some(&status), Some(45), true, None, None, 35_000),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("stale nested config failure must remain pending");

    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_eq!(pauses.get(), 1);
}
