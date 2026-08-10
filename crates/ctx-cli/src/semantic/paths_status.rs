use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{compact_json, config::AppConfig};

use super::{
    health_search::{json_i64, json_string, json_u32},
    runtime_limits::DAEMON_SEMANTIC_JOB_FILE,
};

#[allow(unused_imports)]
pub(super) use ctx_daemon_runtime::*;

pub(super) fn daemon_core_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join("core-refresh.json")
}

pub(super) fn daemon_source_backed_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_core_refresh_job_path(data_root)
}

pub(super) fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(unix))]
pub(super) fn lower_semantic_worker_priority() {}

pub(super) fn daemon_report(data_root: &Path) -> Value {
    daemon_report_with_disabled_status(data_root, true)
}
pub(super) fn daemon_report_with_disabled_status(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let current_config = AppConfig::load(data_root).ok();
    daemon_report_with_config_snapshot(
        data_root,
        disabled_overrides_lifecycle,
        current_config.as_ref(),
    )
}

pub(super) fn daemon_report_with_config(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    current_config: &AppConfig,
) -> Value {
    daemon_report_with_config_snapshot(
        data_root,
        disabled_overrides_lifecycle,
        Some(current_config),
    )
}

fn daemon_report_with_config_snapshot(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    current_config: Option<&AppConfig>,
) -> Value {
    let status_value = read_daemon_status(data_root);
    let enabled = current_config
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|| AppConfig::default().daemon.enabled);
    let daemon_mode = current_config
        .map(|config| config.daemon.mode)
        .or_else(|| {
            status_value
                .as_ref()
                .and_then(|status| status.get("config_reload"))
                .and_then(|reload| reload.get("applied"))
                .and_then(|applied| applied.get("daemon_mode"))
                .and_then(Value::as_str)
                .and_then(crate::config::DaemonMode::parse)
        })
        .unwrap_or_default();
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_value = read_pid_lock_json(&lock_path);
    let lock_pid = read_pid_lock_file(&lock_path);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    let lock_state = lock_pid.map(process_state);
    let lock_reports_running =
        pid_lock_file_reports_running(&lock_path, lock_state, status.as_str());
    let owner_identity_matches = lock_reports_running
        && lock_value.as_ref().is_some_and(|identity| {
            identity
                .get("binary")
                .and_then(Value::as_str)
                .map(Path::new)
                .and_then(|executable| {
                    daemon_owner_binary_identity_matches(identity, executable).ok()
                })
                .unwrap_or(false)
        });
    let owner_identity_mismatch = lock_reports_running && !owner_identity_matches;
    let running = lock_reports_running && owner_identity_matches;
    let stale_lock = lock_path.exists() && pid_lock_file_is_orphaned(&lock_path);
    let stale_lock_overrides_lifecycle = (stale_lock || owner_identity_mismatch)
        && !["completed", "failed"].contains(&status.as_str());
    let stale_running_status = !running && status == "running";
    if running {
        status = "running".to_owned();
    } else if stale_lock_overrides_lifecycle || stale_running_status {
        status = "stale_lock".to_owned();
    } else if !enabled && (disabled_overrides_lifecycle || status == "unknown") {
        status = "disabled".to_owned();
    }
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    let config_reload = daemon_config_reload_report(status_value.as_ref(), running, current_config);
    let semantic_runtime_active = running
        && status_value
            .as_ref()
            .and_then(|value| value.get("semantic_runtime_active"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let start_mode = status_value
        .as_ref()
        .and_then(|value| json_string(value, "start_mode"));
    let trigger_command = status_value
        .as_ref()
        .and_then(|value| json_string(value, "trigger_command"));
    let trigger_provenance = if start_mode.as_deref() == Some("auto") {
        Some("autostart".to_owned())
    } else {
        trigger_command
            .clone()
            .or_else(|| Some("manual".to_owned()))
    };
    let jobs = json!({
        "core_refresh": daemon_core_refresh_job_report(
            data_root,
            disabled_overrides_lifecycle,
            current_config
                .map(|config| config.daemon.enabled)
                .unwrap_or_else(|| AppConfig::default().daemon.enabled),
        ),
        "semantic_index": daemon_semantic_job_report(
            data_root,
            daemon_mode,
            disabled_overrides_lifecycle,
            running,
            semantic_runtime_active,
            &config_reload,
            current_config,
        ),
    });
    let lock_identity = compact_json(json!({
        "path": lock_path,
        "active": running,
        "owner_id": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "owner_id")),
        "pid": lock_pid,
        "binary": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "binary")),
        "binary_sha256": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "binary_sha256")),
        "owner_image_matches": owner_identity_matches,
        "protocol": lock_value
            .as_ref()
            .and_then(|value| json_string(value, "lock_protocol")),
    }));
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "mode": daemon_mode.as_str(),
        "running": running,
        "recoverable": stale_lock_overrides_lifecycle || stale_running_status,
        "reason": if owner_identity_mismatch {
            Some("daemon_owner_identity_mismatch".to_owned())
        } else if stale_lock_overrides_lifecycle {
            Some("daemon_lock_stale".to_owned())
        } else if stale_running_status {
            Some("daemon_status_stale".to_owned())
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "reason"))
        },
        "pid": pid,
        "live_pid": running.then_some(pid).flatten(),
        "started_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
        "heartbeat_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
        "finished_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
        "start_mode": start_mode,
        "trigger_command": trigger_command,
        "trigger_provenance": trigger_provenance,
        "last_error": status_value.as_ref().and_then(|value| json_string(value, "last_error")),
        "semantic_runtime_active": semantic_runtime_active,
        "config_reload": config_reload,
        "lock_path": lock_path,
        "lock_identity": lock_identity,
        "core_refresh_endpoint": daemon_core_refresh_endpoint_report(data_root),
        "supervisor": super::daemon_supervisor::daemon_supervisor_report(data_root),
        "wakeup": super::daemon_wakeup::daemon_wakeup_report(data_root),
        "status_path": status_path,
        "jobs": jobs,
    }))
}

fn daemon_semantic_job_report(
    data_root: &Path,
    daemon_mode: crate::config::DaemonMode,
    disabled_overrides_lifecycle: bool,
    daemon_running: bool,
    semantic_runtime_active: bool,
    config_reload: &Value,
    current_config: Option<&AppConfig>,
) -> Value {
    let requested_daemon_enabled = config_reload
        .pointer("/requested/daemon_enabled")
        .and_then(Value::as_bool);
    let requested_semantic_enabled = config_reload
        .pointer("/requested/semantic_enabled")
        .and_then(Value::as_bool);
    let applied_daemon_enabled = config_reload
        .pointer("/applied/daemon_enabled")
        .and_then(Value::as_bool);
    let applied_semantic_enabled = config_reload
        .pointer("/applied/semantic_enabled")
        .and_then(Value::as_bool);
    let daemon_enabled = requested_daemon_enabled
        .or(applied_daemon_enabled)
        .unwrap_or_else(|| {
            current_config
                .map(|config| config.daemon.enabled)
                .unwrap_or_else(|| AppConfig::default().daemon.enabled)
        });
    let semantic_enabled = requested_semantic_enabled
        .or(applied_semantic_enabled)
        .unwrap_or_else(|| current_config.is_some_and(AppConfig::semantic_search_enabled));
    let semantic_supported = super::semantic_query_service_supported();
    let mode_allows_semantic = !daemon_mode.runs_only_source_refresh();
    let enabled = daemon_enabled && semantic_enabled && semantic_supported && mode_allows_semantic;
    let config_reload_status = config_reload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let config_out_of_sync = config_reload
        .get("out_of_sync")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let activation_failed = config_reload_status == "activation_failed" && semantic_enabled;
    let reload_pending = daemon_running && config_reload_status == "pending" && config_out_of_sync;
    let disabled = !enabled && disabled_overrides_lifecycle && !semantic_runtime_active;
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    let last_run_status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"));
    let last_run_reason = status_value
        .as_ref()
        .and_then(|value| json_string(value, "reason"));
    let status = if activation_failed {
        "failed"
    } else if reload_pending || (daemon_running && enabled && !semantic_runtime_active) {
        "pending"
    } else if disabled {
        "disabled"
    } else {
        last_run_status.as_deref().unwrap_or("unknown")
    };
    let reason = if activation_failed {
        Some("semantic_activation_failed".to_owned())
    } else if reload_pending {
        Some("daemon_config_reload_pending".to_owned())
    } else if daemon_running && enabled && !semantic_runtime_active {
        Some("semantic_runtime_inactive".to_owned())
    } else if disabled {
        Some(if daemon_mode.runs_only_source_refresh() {
            "daemon_mode_source_refresh_only".to_owned()
        } else if !semantic_enabled {
            "semantic_disabled".to_owned()
        } else if !semantic_supported {
            "unsupported_platform".to_owned()
        } else {
            "daemon_disabled".to_owned()
        })
    } else {
        last_run_reason.clone()
    };
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "semantic_enabled": semantic_enabled,
        "daemon_configured": applied_daemon_enabled,
        "semantic_configured": applied_semantic_enabled,
        "runtime_active": semantic_runtime_active,
        "config_reload_status": config_reload_status,
        "configuration_pending": reload_pending,
        "reason": reason,
        "last_run_at_ms": status_value
            .as_ref()
            .and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": last_run_status,
        "last_run_reason": last_run_reason,
        "last_error": if activation_failed {
            config_reload
                .get("last_error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "last_error"))
        },
        "retryable": status_value
            .as_ref()
            .and_then(|value| value.get("retryable").and_then(Value::as_bool)),
        "failure_class": status_value
            .as_ref()
            .and_then(|value| json_string(value, "failure_class")),
        "indexed_chunks": status_value
            .as_ref()
            .and_then(|value| value.get("indexed_chunks").and_then(Value::as_u64)),
        "model_key": status_value
            .as_ref()
            .and_then(|value| json_string(value, "model_key")),
        "daemon_mode": daemon_mode.as_str(),
    }))
}

pub(super) fn daemon_core_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
    daemon_enabled: bool,
) -> Value {
    let status_value = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let job = status_value.as_ref();
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    compact_json(json!({
        "status": if disabled {
            "disabled"
        } else {
            job.and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        },
        "enabled": daemon_enabled,
        "reason": if disabled {
            Some("daemon_disabled".to_owned())
        } else {
            job.and_then(|value| json_string(value, "reason"))
        },
        "error_code": job.and_then(|value| json_string(value, "error_code")),
        "mode": job.and_then(|value| json_string(value, "mode")),
        "owner": job.and_then(|value| json_string(value, "owner")),
        "kind": job.and_then(|value| json_string(value, "kind")),
        "request_id": job.and_then(|value| json_string(value, "request_id")),
        "request_state": job.and_then(|value| json_string(value, "request_state")),
        "last_run_at_ms": job.and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": job.and_then(|value| value.get("source_count").cloned()),
        "previous_generation": job
            .and_then(|value| json_string(value, "previous_generation")),
        "published_generation": job
            .and_then(|value| json_string(value, "published_generation")),
        "generation_changed": job
            .and_then(|value| value.get("generation_changed").cloned()),
        "receipt": job.and_then(|value| value.get("receipt").cloned()),
        "coalesced_requests": job
            .and_then(|value| value.get("coalesced_requests").cloned()),
        "progress": job.and_then(|value| value.get("progress").cloned()),
        "daemon_mode": job.and_then(|value| json_string(value, "daemon_mode")),
        "trigger": job.and_then(|value| json_string(value, "trigger")),
        "trigger_provenance": job
            .and_then(|value| json_string(value, "trigger_provenance")),
        "scanned_routes": job.and_then(|value| value.get("scanned_routes").cloned()),
        "unsupported_routes": job
            .and_then(|value| value.get("unsupported_routes").cloned()),
        "certified_source_count": job
            .and_then(|value| value.get("certified_source_count").cloned()),
        "certified_source_bytes": job
            .and_then(|value| value.get("certified_source_bytes").cloned()),
        "timings_us": job.and_then(|value| value.get("timings_us").cloned()),
        "retryable": job.and_then(|value| value.get("retryable").cloned()),
        "retry_after_ms": job.and_then(|value| value.get("retry_after_ms").cloned()),
        "consecutive_failures": job
            .and_then(|value| value.get("consecutive_failures").cloned()),
        "retry_not_before_at_ms": job
            .and_then(|value| value.get("retry_not_before_at_ms").cloned()),
        "last_error": job.and_then(|value| json_string(value, "last_error")),
    }))
}

fn daemon_core_refresh_endpoint_report(data_root: &Path) -> Value {
    let identity_path = daemon_root_path(data_root).join("source-refresh-endpoint.json");
    let identity = fs::read_to_string(&identity_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    compact_json(json!({
        "identity_path": identity_path,
        "available": identity.is_some(),
        "transport": identity
            .as_ref()
            .and_then(|value| json_string(value, "transport")),
        "owner_pid": identity.as_ref().and_then(|value| json_u32(value, "pid")),
        "address": identity.as_ref().and_then(|value| {
            json_string(value, "path").or_else(|| json_string(value, "pipe_name"))
        }),
    }))
}

fn daemon_config_reload_report(
    daemon_status: Option<&Value>,
    running: bool,
    current_config: Option<&AppConfig>,
) -> Value {
    let persisted = daemon_status
        .and_then(|value| value.get("config_reload"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let applied_daemon_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_enabled"))
        .and_then(Value::as_bool);
    let applied_daemon_mode = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_mode"))
        .and_then(Value::as_str);
    let applied_semantic_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let requested_daemon_enabled = current_config.map(|config| config.daemon.enabled);
    let requested_daemon_mode = current_config.map(|config| config.daemon.mode.as_str());
    let requested_semantic_enabled = current_config.map(AppConfig::semantic_search_enabled);
    let out_of_sync = running
        && (requested_daemon_enabled != applied_daemon_enabled
            || requested_daemon_mode != applied_daemon_mode
            || requested_semantic_enabled != applied_semantic_enabled);
    let persisted_status = persisted
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = if out_of_sync && persisted_status == "applied" {
        "pending"
    } else {
        persisted_status
    };
    let reason = if out_of_sync && persisted_status == "applied" {
        Some("config_changed")
    } else {
        None
    };

    compact_json(json!({
        "status": status,
        "reason": reason,
        "out_of_sync": out_of_sync,
        "last_attempt_at_ms": persisted.get("last_attempt_at_ms").cloned(),
        "last_applied_at_ms": persisted.get("last_applied_at_ms").cloned(),
        "requested": {
            "daemon_enabled": requested_daemon_enabled,
            "daemon_mode": requested_daemon_mode,
            "semantic_enabled": requested_semantic_enabled,
        },
        "applied": {
            "daemon_enabled": applied_daemon_enabled,
            "daemon_mode": applied_daemon_mode,
            "semantic_enabled": applied_semantic_enabled,
        },
        "last_error": persisted.get("last_error").cloned(),
    }))
}

#[cfg(test)]
mod tests;
