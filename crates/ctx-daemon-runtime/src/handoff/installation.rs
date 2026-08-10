use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::{
    open_or_create_pid_lock_file, process_state, secure_private_file_permissions, ProcessState,
};

pub struct InstallationQuiescence {
    lock: fs::File,
}

impl Drop for InstallationQuiescence {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

pub fn open_installation_quiescence_lock(path: &Path) -> Result<fs::File> {
    let (file, _) = open_or_create_pid_lock_file(path)
        .with_context(|| format!("open ctx installation daemon lock {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(file)
}

pub fn try_acquire_installation_quiescence(path: &Path) -> Result<Option<InstallationQuiescence>> {
    let lock = open_installation_quiescence_lock(path)?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(InstallationQuiescence { lock })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("acquire ctx installation daemon quiescence"),
    }
}

pub fn wait_for_installation_quiescence(
    lock_path: &Path,
    registration_root: &Path,
    attempt_id: &str,
    timeout: Duration,
    poll_interval: Duration,
    idle_exit_cap: u64,
    loop_interval_cap: u64,
) -> Result<()> {
    let lock = open_installation_quiescence_lock(lock_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out waiting for all ctx daemons to acknowledge installation quiescence"
                    ));
                }
                std::thread::sleep(poll_interval);
            }
            Err(error) => {
                return Err(error).context("acquire ctx installation daemon quiescence");
            }
        }
    }
    let result = read_installation_restart_records(
        registration_root,
        attempt_id,
        true,
        idle_exit_cap,
        loop_interval_cap,
    )
    .map(|_| ());
    let _ = fs2::FileExt::unlock(&lock);
    result
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InstallationRestartRecord {
    pub registration_path: PathBuf,
    pub data_root: PathBuf,
    pub opaque_trigger: String,
    pub idle_exit_seconds: Option<u64>,
    pub loop_interval_seconds: Option<u64>,
}

pub fn read_installation_restart_records(
    root: &Path,
    attempt_id: &str,
    fail_on_live: bool,
    idle_exit_cap: u64,
    loop_interval_cap: u64,
) -> Result<Vec<InstallationRestartRecord>> {
    let registrations = read_installation_registrations(root)?;
    let mut restarts = Vec::new();
    for (path, value) in registrations {
        let status = value["status"].as_str().unwrap_or_default();
        let registration_attempt = value.get("attempt_id").and_then(Value::as_str);
        if status == "quiescing" && registration_attempt == Some(attempt_id) {
            return Err(anyhow!(
                "ctx daemon exited without completing its quiescence acknowledgement"
            ));
        }
        if status == "live" {
            let pid = value["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or_default();
            if fail_on_live
                && matches!(
                    process_state(pid),
                    ProcessState::Running | ProcessState::Unknown
                )
            {
                return Err(anyhow!(
                    "ctx daemon registration remains live after installation quiescence"
                ));
            }
            continue;
        }
        if status != "acknowledged" || registration_attempt != Some(attempt_id) {
            continue;
        }
        let data_root = PathBuf::from(value["data_root"].as_str().unwrap_or_default());
        let opaque_trigger = value["trigger_command"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid trigger"))?
            .to_owned();
        let persistent = value
            .get("persistent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let idle_exit_seconds = if persistent {
            None
        } else {
            Some(
                value["idle_exit_seconds"]
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= idle_exit_cap)
                    .ok_or_else(|| {
                        anyhow!("ctx daemon acknowledgement has an invalid idle timeout")
                    })?,
            )
        };
        let loop_interval_explicit = value
            .get("loop_interval_explicit")
            .and_then(Value::as_bool)
            .unwrap_or(!persistent);
        let loop_interval_seconds = match value["loop_interval_seconds"].as_u64() {
            Some(value) if value > 0 && value <= loop_interval_cap && loop_interval_explicit => {
                Some(value)
            }
            Some(value) if value > 0 && value <= loop_interval_cap && persistent => None,
            _ => {
                return Err(anyhow!(
                    "ctx daemon acknowledgement has an invalid loop interval"
                ));
            }
        };
        restarts.push(InstallationRestartRecord {
            registration_path: path,
            data_root,
            opaque_trigger,
            idle_exit_seconds,
            loop_interval_seconds,
        });
    }
    restarts.sort_by(|left, right| left.data_root.cmp(&right.data_root));
    restarts.dedup_by(|left, right| left.data_root == right.data_root);
    Ok(restarts)
}

pub fn read_installation_registrations(root: &Path) -> Result<Vec<(PathBuf, Value)>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read ctx daemon acknowledgements {}", root.display()));
        }
    };
    let mut registrations = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect ctx daemon acknowledgement {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "ctx daemon acknowledgement is not a regular file: {}",
                path.display()
            ));
        }
        let value = fs::read(&path)
            .with_context(|| format!("read ctx daemon acknowledgement {}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes)
                    .with_context(|| format!("parse ctx daemon acknowledgement {}", path.display()))
            })?;
        validate_installation_registration(&value, &path)?;
        registrations.push((path, value));
    }
    registrations.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(registrations)
}

fn validate_installation_registration(value: &Value, path: &Path) -> Result<()> {
    let valid_status = matches!(
        value.get("status").and_then(Value::as_str),
        Some("live" | "released" | "quiescing" | "acknowledged")
    );
    let valid_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .is_some_and(Path::is_absolute);
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || !value
            .get("registration_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
        || !valid_status
        || !valid_root
    {
        return Err(anyhow!(
            "invalid ctx daemon acknowledgement at {}",
            path.display()
        ));
    }
    Ok(())
}

pub fn registered_installation_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = read_installation_registrations(root)?
        .into_iter()
        .map(|(_, value)| PathBuf::from(value["data_root"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    Ok(roots)
}
