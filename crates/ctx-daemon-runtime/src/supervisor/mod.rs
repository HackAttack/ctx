use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Error, Result};

mod artifact;
mod lock;
mod platform;
mod state;
mod windows;

pub use artifact::*;
pub use lock::SupervisorInstallationLock;
pub use platform::*;
pub use state::write_atomic_supervisor_file;
pub use windows::*;

const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub trait NativeSupervisorBackend<E>: Sync {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn install(&self, data_root: &Path, executable: &Path, environment: &E) -> Result<PathBuf>;
    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()>;
    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32>;
    fn start(&self, data_root: &Path) -> Result<()>;
}

#[derive(Debug)]
pub enum SupervisorEnsureOutcome {
    Native {
        artifact: Option<PathBuf>,
        owner_pid: u32,
        environment_installed: bool,
    },
    RegisteredNotRunning {
        artifact: Option<PathBuf>,
        initial_error: Error,
        recovery_error: Error,
        environment_installed: bool,
    },
    InstallFailed {
        artifact: Option<PathBuf>,
        error: Error,
        cleanup_error: Option<Error>,
    },
}

pub fn ensure_native_supervisor_with<E>(
    data_root: &Path,
    executable: &Path,
    environment: &E,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Result<SupervisorEnsureOutcome> {
    let artifact = backend.artifact_path(data_root)?;
    if backend.verify_registration(data_root, executable).is_ok() {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => {
                return Ok(SupervisorEnsureOutcome::Native {
                    artifact,
                    owner_pid,
                    environment_installed: false,
                });
            }
            Err(initial_live_error) => {
                return match backend
                    .start(data_root)
                    .and_then(|()| wait_for_native_live_owner(data_root, executable, backend))
                {
                    Ok(owner_pid) => Ok(SupervisorEnsureOutcome::Native {
                        artifact,
                        owner_pid,
                        environment_installed: false,
                    }),
                    Err(recovery_error) => Ok(SupervisorEnsureOutcome::RegisteredNotRunning {
                        artifact,
                        initial_error: initial_live_error,
                        recovery_error,
                        environment_installed: false,
                    }),
                };
            }
        }
    }

    let installation = backend
        .install(data_root, executable, environment)
        .and_then(|installed_artifact| {
            wait_for_native_live_owner(data_root, executable, backend)
                .map(|owner_pid| (installed_artifact, owner_pid))
        });
    match installation {
        Ok((installed_artifact, owner_pid)) => Ok(SupervisorEnsureOutcome::Native {
            artifact: Some(installed_artifact),
            owner_pid,
            environment_installed: true,
        }),
        Err(error) if backend.verify_registration(data_root, executable).is_ok() => {
            let recovery = backend
                .verify_live_owner(data_root, executable)
                .or_else(|_| {
                    backend.start(data_root)?;
                    wait_for_native_live_owner(data_root, executable, backend)
                });
            match recovery {
                Ok(owner_pid) => Ok(SupervisorEnsureOutcome::Native {
                    artifact,
                    owner_pid,
                    environment_installed: true,
                }),
                Err(recovery_error) => Ok(SupervisorEnsureOutcome::RegisteredNotRunning {
                    artifact,
                    initial_error: error,
                    recovery_error,
                    environment_installed: true,
                }),
            }
        }
        Err(error) => {
            let cleanup_error = backend.disable(data_root).err();
            Ok(SupervisorEnsureOutcome::InstallFailed {
                artifact: if cleanup_error.is_some() {
                    backend.artifact_path(data_root)?
                } else {
                    None
                },
                error,
                cleanup_error,
            })
        }
    }
}

fn wait_for_native_live_owner<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Result<u32> {
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    loop {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => return Ok(owner_pid),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "native supervisor did not start daemon lifecycle ownership"
            ));
        }
        std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
    }
}

pub trait SupervisorUpgradeFence {
    fn release(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub enum SupervisorResumeOutcome {
    Native {
        artifact: Option<PathBuf>,
        owner_pid: u32,
    },
    Fallback,
    RegisteredNotRunning {
        artifact: Option<PathBuf>,
        error: Error,
    },
}

pub fn resume_native_supervisor_with<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
    upgrade_fence: &mut dyn SupervisorUpgradeFence,
) -> Result<SupervisorResumeOutcome> {
    if backend.verify_registration(data_root, executable).is_err() {
        return Ok(SupervisorResumeOutcome::Fallback);
    }
    upgrade_fence.release()?;
    let owner = backend
        .verify_live_owner(data_root, executable)
        .or_else(|_| {
            backend.start(data_root)?;
            wait_for_native_live_owner(data_root, executable, backend)
        });
    match owner {
        Ok(owner_pid) => Ok(SupervisorResumeOutcome::Native {
            artifact: backend.artifact_path(data_root)?,
            owner_pid,
        }),
        Err(error) => Ok(SupervisorResumeOutcome::RegisteredNotRunning {
            artifact: backend.artifact_path(data_root)?,
            error,
        }),
    }
}
