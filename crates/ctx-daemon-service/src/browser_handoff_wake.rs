use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths_status::{
    create_private_dir_all, daemon_jobs_path, open_or_create_pid_lock_file,
    secure_private_file_permissions, write_daemon_job_status,
};

const PENDING_FILE: &str = "browser-handoff-pending.json";
const PENDING_LOCK_FILE: &str = "browser-handoff-pending.lock";
const PENDING_SCHEMA_VERSION: u16 = 1;
const MAX_PENDING_MARKER_BYTES: u64 = 1_024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserHandoffPending {
    schema_version: u16,
    request_id: Uuid,
}

impl BrowserHandoffPending {
    fn new() -> Self {
        Self {
            schema_version: PENDING_SCHEMA_VERSION,
            request_id: Uuid::now_v7(),
        }
    }

    fn validate(self) -> Result<Self> {
        if self.schema_version != PENDING_SCHEMA_VERSION || self.request_id.is_nil() {
            anyhow::bail!("invalid browser-handoff pending marker");
        }
        Ok(self)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct BrowserHandoffMarkerRevision(Vec<u8>);

impl std::fmt::Debug for BrowserHandoffMarkerRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserHandoffMarkerRevision")
            .field("bytes", &self.0.len())
            .finish()
    }
}

pub(crate) struct BrowserHandoffMarkerSnapshot {
    bytes: Vec<u8>,
}

impl BrowserHandoffMarkerSnapshot {
    pub(crate) fn revision(&self) -> BrowserHandoffMarkerRevision {
        BrowserHandoffMarkerRevision(self.bytes.clone())
    }

    pub(crate) fn marker(&self) -> Result<BrowserHandoffPending> {
        if self.bytes.len() > MAX_PENDING_MARKER_BYTES as usize {
            anyhow::bail!("browser-handoff pending marker is oversized");
        }
        serde_json::from_slice::<BrowserHandoffPending>(&self.bytes)
            .context("parse browser-handoff pending marker")?
            .validate()
    }
}

/// Durably records that the daemon should inspect the secret-backed handoff
/// schedule. The marker itself contains no commercial credential material.
pub fn publish_browser_handoff_pending(data_root: &Path) -> Result<()> {
    let marker = BrowserHandoffPending::new();
    with_lock(data_root, || {
        write_daemon_job_status(&path(data_root), &serde_json::to_value(marker)?)
    })
}

/// Reads the marker only when it exists. The ordinary no-handoff path is a
/// single filesystem existence check and never reaches the credential vault.
#[cfg(test)]
pub(crate) fn observe(data_root: &Path) -> Result<Option<BrowserHandoffPending>> {
    observe_snapshot(data_root)?
        .map(|snapshot| snapshot.marker())
        .transpose()
}

/// Returns the bounded non-secret marker bytes even when their JSON is
/// corrupt. The scheduler uses this revision to quarantine one bad marker
/// while immediately recognizing a replacement published by explicit setup.
pub(crate) fn observe_snapshot(data_root: &Path) -> Result<Option<BrowserHandoffMarkerSnapshot>> {
    if !path(data_root).exists() {
        return Ok(None);
    }
    with_lock(data_root, || read_snapshot_unlocked(data_root))
}

/// Removes only the marker observed before the vault inspection. A concurrent
/// explicit handoff creation publishes a new identity and cannot lose its wake.
pub(crate) fn complete(data_root: &Path, observed: &BrowserHandoffPending) -> Result<bool> {
    with_lock(data_root, || {
        let current = read_snapshot_unlocked(data_root)?
            .map(|snapshot| snapshot.marker())
            .transpose()?;
        if current.as_ref() != Some(observed) {
            return Ok(false);
        }
        fs::remove_file(path(data_root)).with_context(|| {
            format!(
                "complete browser-handoff pending marker {}",
                path(data_root).display()
            )
        })?;
        Ok(true)
    })
}

fn read_snapshot_unlocked(data_root: &Path) -> Result<Option<BrowserHandoffMarkerSnapshot>> {
    let marker_path = path(data_root);
    let file = match fs::File::open(&marker_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read browser-handoff marker {}", marker_path.display()))
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_PENDING_MARKER_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read browser-handoff marker {}", marker_path.display()))?;
    Ok(Some(BrowserHandoffMarkerSnapshot { bytes }))
}

fn with_lock<T>(data_root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let jobs = daemon_jobs_path(data_root);
    create_private_dir_all(&jobs)?;
    let lock_path = lock_path(data_root);
    let (lock, _) = open_or_create_pid_lock_file(&lock_path)
        .with_context(|| format!("open browser-handoff marker lock {}", lock_path.display()))?;
    secure_private_file_permissions(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("lock browser-handoff marker {}", lock_path.display()))?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&lock)
        .with_context(|| format!("unlock browser-handoff marker {}", lock_path.display()));
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

pub(crate) fn path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(PENDING_FILE)
}

fn lock_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(PENDING_LOCK_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_does_not_remove_a_newer_explicit_wake() -> Result<()> {
        let temp = tempfile::tempdir()?;
        publish_browser_handoff_pending(temp.path())?;
        let first = observe(temp.path())?.expect("first marker");
        publish_browser_handoff_pending(temp.path())?;

        assert!(!complete(temp.path(), &first)?);
        assert!(observe(temp.path())?.is_some());
        Ok(())
    }
}
