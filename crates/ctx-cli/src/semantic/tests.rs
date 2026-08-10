use std::{fs, process, sync::Arc, time::Duration as StdDuration};

use anyhow::Result;
use ctx_daemon_runtime::{
    create_private_dir_all, daemon_lock_is_stale, daemon_lock_path, daemon_status_path,
    observe_pid_advisory_lock, pid_lock_file_reports_running, pid_lock_guard_path,
    pid_lock_payload, pid_lock_uses_advisory_protocol, private_create_new_lock_file,
    private_open_existing_lock_file, publish_pid_lock_metadata, read_pid_lock_json,
    write_private_json_file, DaemonLock, PidAdvisoryLockObservation, ProcessState,
};
use ctx_history_core::utc_now;
use serde_json::json;

use super::paths_status;

fn write_test_daemon_lifecycle_status(
    data_root: &std::path::Path,
    status: &str,
    last_error: Option<String>,
) -> Result<()> {
    write_private_json_file(
        &daemon_status_path(data_root),
        &json!({
            "schema_version": 1,
            "status": status,
            "pid": 123,
            "started_at_ms": 123,
            "heartbeat_at_ms": 456,
            "finished_at_ms": 456,
            "start_mode": "auto",
            "trigger_command": "setup",
            "last_error": last_error,
        }),
    )
}

mod lifecycle;
mod locking;
