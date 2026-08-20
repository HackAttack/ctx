use std::{
    fs,
    io::Write as _,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::io::Read as _;

use anyhow::{anyhow, Context, Result};

use crate::replace_private_file;

static SUPERVISOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn write_atomic_supervisor_file(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("supervisor artifact has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create supervisor artifact directory {}", parent.display()))?;
    let sequence = SUPERVISOR_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ctx-supervisor"),
        std::process::id(),
        sequence,
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .with_context(|| format!("create supervisor artifact {}", temp.display()))?;
        file.write_all(body)
            .with_context(|| format!("write supervisor artifact {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync supervisor artifact {}", temp.display()))?;
        drop(file);
        replace_private_file(&temp, path)
            .with_context(|| format!("publish supervisor artifact {}", path.display()))?;
        sync_supervisor_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn write_atomic_supervisor_file_if_changed(path: &Path, body: &[u8]) -> Result<bool> {
    if existing_supervisor_file_is_current(path, body)? {
        return Ok(false);
    }
    write_atomic_supervisor_file(path, body)?;
    Ok(true)
}

#[cfg(unix)]
fn existing_supervisor_file_is_current(path: &Path, body: &[u8]) -> Result<bool> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(false)
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open supervisor artifact {}", path.display()))
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect opened supervisor artifact {}", path.display()))?;
    if !metadata.is_file()
        || metadata.len() != u64::try_from(body.len()).unwrap_or(u64::MAX)
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Ok(false);
    }
    let mut installed = Vec::with_capacity(body.len());
    file.read_to_end(&mut installed)
        .with_context(|| format!("read opened supervisor artifact {}", path.display()))?;
    if installed != body {
        return Ok(false);
    }
    if metadata.permissions().mode() & 0o7777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| {
                format!("repair supervisor artifact {} permissions", path.display())
            })?;
        file.sync_all()
            .with_context(|| format!("sync repaired supervisor artifact {}", path.display()))?;
    }
    let repaired = file
        .metadata()
        .with_context(|| format!("reinspect opened supervisor artifact {}", path.display()))?;
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reinspect supervisor artifact {}", path.display()))
        }
    };
    Ok(path_metadata.is_file()
        && !path_metadata.file_type().is_symlink()
        && repaired.dev() == path_metadata.dev()
        && repaired.ino() == path_metadata.ino()
        && repaired.uid() == unsafe { libc::geteuid() }
        && repaired.nlink() == 1
        && repaired.permissions().mode() & 0o7777 == 0o600
        && path_metadata.uid() == unsafe { libc::geteuid() }
        && path_metadata.nlink() == 1
        && path_metadata.permissions().mode() & 0o7777 == 0o600)
}

#[cfg(not(unix))]
fn existing_supervisor_file_is_current(_path: &Path, _body: &[u8]) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn sync_supervisor_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open supervisor directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync supervisor directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_supervisor_directory(_path: &Path) -> Result<()> {
    Ok(())
}
