use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use super::{ensure_directory, is_not_found, Entry, Layout, Slot};

pub(crate) fn candidate_root(install_root: &Path, attempt_id: &str) -> Result<PathBuf> {
    if !crate::journal::valid_attempt_id(attempt_id) {
        bail!("managed-pair attempt ID is invalid");
    }
    Ok(install_root
        .join("share/ctx/.managed-pair-candidates")
        .join(attempt_id))
}

pub(crate) fn create_candidate(install_root: &Path, attempt_id: &str) -> Result<PathBuf> {
    let base = install_root.join("share/ctx/.managed-pair-candidates");
    ensure_directory(&base)?;
    let root = candidate_root(install_root, attempt_id)?;
    ensure_directory(&root)?;
    Layout::open(&root, true)?;
    Ok(root)
}

pub(crate) fn candidate_exists(layout: &Layout, attempt_id: &str) -> Result<bool> {
    match layout.open_candidate_attempt(attempt_id) {
        Ok(_) => Ok(true),
        Err(error) if is_not_found(&error) => Ok(false),
        Err(error) => Err(error).context("inspect managed-pair candidate root"),
    }
}

pub(crate) fn remove_candidate(layout: &Layout, attempt_id: &str) -> Result<()> {
    if !candidate_exists(layout, attempt_id)? {
        return Ok(());
    }
    let (candidate, base) = layout.open_candidate_attempt(attempt_id)?;
    for slot in Slot::ALL {
        let entry = candidate.target(slot);
        match entry.directory.entry_metadata(&entry.name, entry.path())? {
            Some(metadata) if metadata.is_file || metadata.is_symlink => {
                entry.directory.remove_file(&entry.name, entry.path())?;
                entry.directory.sync()?;
            }
            Some(_) => bail!("managed-pair candidate {} is unsafe", slot.label()),
            None => {}
        }
    }
    candidate
        .share_directory
        .remove_directory(OsStr::new("ctx"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("share"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("libexec"))?;
    candidate
        .root_directory
        .remove_directory(OsStr::new("bin"))?;
    base.remove_directory(OsStr::new(attempt_id))?;
    base.sync()?;
    Ok(())
}

pub(crate) fn legacy_journal_present(install_root: &Path) -> Result<bool> {
    for name in [
        ".managed-pair-transaction.json",
        ".managed-pair-bootstrap-transaction-v1.json",
    ] {
        let path = install_root.join("share/ctx").join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                return Ok(true)
            }
            Ok(_) => bail!("bootstrap managed-pair transaction journal is unsafe"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("inspect bootstrap managed-pair transaction journal")
            }
        }
    }
    Ok(false)
}

pub(super) fn transaction_sibling(target: &Entry, attempt_id: &str, suffix: &str) -> Entry {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-pair");
    target.sibling(format!(".{name}.managed-pair-{attempt_id}.{suffix}").into())
}
