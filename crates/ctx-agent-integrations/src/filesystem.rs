use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use fs2::FileExt as _;
use uuid::Uuid;

mod security_metadata;

pub(crate) fn atomic_update(
    path: &Path,
    update: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    reject_unsafe_target(path)?;

    let lock_path = lock_path(path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open transaction lock {}", lock_path.display()))?;
    lock.lock_exclusive()
        .with_context(|| format!("lock transaction {}", lock_path.display()))?;
    reject_unsafe_target(path)?;

    let existing = match fs::read(path) {
        Ok(body) => Some(body),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let replacement = update(existing.as_deref())?;
    if existing.as_deref() == Some(replacement.as_slice()) {
        return Ok(());
    }

    let stage = stage_path(path)?;
    let result = publish(path, &stage, existing.is_some(), &replacement);
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}

fn publish(path: &Path, stage: &Path, replacing: bool, body: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o666);
    }
    let mut file = options
        .open(stage)
        .with_context(|| format!("create staged file {}", stage.display()))?;
    if replacing {
        security_metadata::copy_existing_security(path, &file)
            .with_context(|| format!("preserve security metadata for {}", path.display()))?;
    }
    file.write_all(body)
        .with_context(|| format!("write staged file {}", stage.display()))?;
    file.sync_all()
        .with_context(|| format!("sync staged file {}", stage.display()))?;
    drop(file);
    replace_file(stage, path, replacing).with_context(|| format!("publish {}", path.display()))?;
    sync_directory(path.parent().expect("validated parent"))
}

fn reject_unsafe_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(anyhow!("refusing to replace symlink {}", path.display()))
        }
        Ok(_) => Err(anyhow!(
            "refusing to replace non-regular file {}",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn stage_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("target path has no file name: {}", path.display()))?;
    Ok(path.with_file_name(format!(
        ".{}.ctx-stage-{}",
        name.to_string_lossy(),
        Uuid::new_v4()
    )))
}

fn lock_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("target path has no file name: {}", path.display()))?;
    Ok(path.with_file_name(format!(
        ".{}.ctx-agent-integrations.lock",
        name.to_string_lossy()
    )))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path, _replacing: bool) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path, replacing: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACEFILE_WRITE_THROUGH,
    };

    let source = wide_path(source);
    let target = wide_path(target);
    let replaced = unsafe {
        if replacing {
            ReplaceFileW(
                target.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_is_atomic_and_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        atomic_update(&path, |_| Ok(b"first".to_vec())).unwrap();
        atomic_update(&path, |existing| {
            assert_eq!(existing, Some(b"first".as_slice()));
            Ok(b"second".to_vec())
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, root.path().join("link")).unwrap();
            assert!(atomic_update(&root.path().join("link"), |_| Ok(Vec::new())).is_err());
        }
    }
}
