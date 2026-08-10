use std::path::{Path, PathBuf};

pub use ctx_daemon_runtime::*;

use super::runtime_limits::DAEMON_SEMANTIC_JOB_FILE;

pub fn daemon_core_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join("core-refresh.json")
}

pub fn daemon_source_backed_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_core_refresh_job_path(data_root)
}

pub fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(unix))]
pub(crate) fn lower_semantic_worker_priority() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_job_paths_preserve_the_existing_persisted_contract() {
        let data_root = Path::new("ctx-data");
        assert_eq!(
            daemon_core_refresh_job_path(data_root),
            Path::new("ctx-data/daemon/jobs/core-refresh.json")
        );
        assert_eq!(
            daemon_semantic_job_path(data_root),
            Path::new("ctx-data/daemon/jobs/semantic-index.json")
        );
    }
}
