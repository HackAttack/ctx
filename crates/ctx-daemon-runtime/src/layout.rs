use std::path::{Path, PathBuf};

pub const DAEMON_DIR: &str = "daemon";
pub const DAEMON_JOBS_DIR: &str = "jobs";
pub const DAEMON_LOCK_FILE: &str = "daemon.lock";
pub const DAEMON_STATUS_FILE: &str = "status.json";
pub const DAEMON_QUERY_SOCKET_FILE: &str = "query.sock";
pub const DAEMON_UPGRADE_HANDOFF_FILE: &str = "upgrade-handoff.json";
pub const DAEMON_UPGRADE_RESTART_REQUEST_DIR: &str = "upgrade-restart-requests";

pub fn daemon_root_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_DIR)
}

pub fn daemon_jobs_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_JOBS_DIR)
}

pub fn daemon_lock_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_LOCK_FILE)
}

pub fn daemon_status_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_STATUS_FILE)
}

#[cfg(unix)]
pub fn daemon_query_socket_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_SOCKET_FILE)
}

pub fn daemon_upgrade_handoff_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_HANDOFF_FILE)
}

pub fn daemon_upgrade_restart_request_root(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_RESTART_REQUEST_DIR)
}
