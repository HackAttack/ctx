use std::path::{Path, PathBuf};

pub use ctx_daemon_runtime::*;

pub fn daemon_core_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join("core-refresh.json")
}

pub fn daemon_source_backed_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_core_refresh_job_path(data_root)
}

pub fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join("semantic.json")
}

#[cfg(unix)]
pub(crate) fn lower_semantic_worker_priority() {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(windows)]
pub(crate) fn lower_semantic_worker_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn lower_semantic_worker_priority() {}
