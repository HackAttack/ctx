use std::{fs, process, sync::Arc, time::Duration as StdDuration};

use anyhow::Result;
use ctx_daemon_runtime::{
    create_private_dir_all, daemon_lock_is_stale, daemon_lock_path, observe_pid_advisory_lock,
    pid_lock_file_reports_running, pid_lock_guard_path, pid_lock_payload,
    pid_lock_uses_advisory_protocol, private_open_existing_lock_file, publish_pid_lock_metadata,
    read_pid_lock_json, DaemonLock, PidAdvisoryLockObservation, ProcessState,
};
use ctx_history_core::utc_now;
use serde_json::json;

mod locking;
