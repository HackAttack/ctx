use std::{
    io,
    process::Child,
    time::{Duration, Instant},
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplacementChildWaitError {
    #[error(transparent)]
    ChildStatus(#[from] io::Error),
    #[error("replacement ctx daemon exited before acquiring lifecycle ownership")]
    ExitedBeforeOwnership,
    #[error("timed out waiting for the replacement ctx daemon to start")]
    TimedOut,
}

#[derive(Debug, Error)]
pub enum DaemonReadinessWaitError {
    #[error("replacement ctx daemon exited before lifecycle readiness")]
    ExitedBeforeReadiness,
    #[error("timed out waiting for replacement ctx daemon readiness")]
    TimedOut,
}

pub fn wait_for_replacement_child(
    child: &mut Child,
    timeout: Duration,
    poll_interval: Duration,
    mut owns_lifecycle: impl FnMut(u32) -> bool,
) -> Result<(), ReplacementChildWaitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if owns_lifecycle(child.id()) {
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            return Err(ReplacementChildWaitError::ExitedBeforeOwnership);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ReplacementChildWaitError::TimedOut);
        }
        std::thread::sleep(poll_interval);
    }
}

pub fn wait_for_daemon_ready(
    timeout: Duration,
    poll_interval: Duration,
    mut owner_active: impl FnMut() -> bool,
    mut restart_request_pending: impl FnMut() -> bool,
) -> Result<(), DaemonReadinessWaitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if owner_active() && !restart_request_pending() {
            return Ok(());
        }
        if !owner_active() {
            return Err(DaemonReadinessWaitError::ExitedBeforeReadiness);
        }
        if Instant::now() >= deadline {
            return Err(DaemonReadinessWaitError::TimedOut);
        }
        std::thread::sleep(poll_interval);
    }
}
