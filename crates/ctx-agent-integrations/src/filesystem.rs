use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt as _;
use uuid::Uuid;

mod security_metadata;

struct ObservedTarget {
    body: Vec<u8>,
    identity: FileIdentity,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity;

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

    let existing = open_observed_target(path)?;
    let replacement = update(existing.as_ref().map(|target| target.body.as_slice()))?;
    if existing
        .as_ref()
        .is_some_and(|target| target.body == replacement)
    {
        return Ok(());
    }

    let stage = stage_path(path)?;
    publish(path, &stage, existing.as_ref(), &replacement)
}

fn publish(
    path: &Path,
    stage: &Path,
    existing: Option<&ObservedTarget>,
    body: &[u8],
) -> Result<()> {
    let mut stage_cleanup = StageCleanup::new(stage);
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
    file.write_all(body)
        .with_context(|| format!("write staged file {}", stage.display()))?;
    file.sync_all()
        .with_context(|| format!("sync staged file {}", stage.display()))?;

    if let Some(existing) = existing {
        let current = verify_target_unchanged(path, existing)?;
        security_metadata::copy_existing_security(&current, &file)
            .with_context(|| format!("preserve security metadata for {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync staged metadata {}", stage.display()))?;
    }
    drop(file);
    // Deliberately after the last path-based validation: tests use this point
    // to exercise the otherwise tiny check-to-publication race.
    run_before_publish_hook(path);
    publish_stage(stage, path, existing, &mut stage_cleanup)
        .with_context(|| format!("publish {}", path.display()))?;
    stage_cleanup.disarm();
    sync_directory(path.parent().expect("validated parent"))
}

struct StageCleanup<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> StageCleanup<'a> {
    const fn new(path: &'a Path) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StageCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(self.path);
        }
    }
}

fn open_observed_target(path: &Path) -> Result<Option<ObservedTarget>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow!("refusing to replace symlink {}", path.display()));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(anyhow!(
                "refusing to replace non-regular file {}",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", path.display()));
        }
    }

    let mut file = open_regular_nofollow(path)
        .with_context(|| format!("open existing target {}", path.display()))?;
    let identity = file_identity(&file)
        .with_context(|| format!("identify existing target {}", path.display()))?;
    let mut body = Vec::new();
    file.read_to_end(&mut body)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Some(ObservedTarget { body, identity }))
}

fn verify_target_unchanged(path: &Path, existing: &ObservedTarget) -> Result<File> {
    let mut current = open_regular_nofollow(path)
        .with_context(|| format!("reopen target before publish {}", path.display()))?;
    let identity = file_identity(&current)
        .with_context(|| format!("identify target before publish {}", path.display()))?;
    if identity != existing.identity || !contents_equal(&mut current, &existing.body)? {
        return Err(anyhow!(
            "refusing to overwrite concurrently changed target {}",
            path.display()
        ));
    }
    Ok(current)
}

fn contents_equal(file: &mut File, expected: &[u8]) -> io::Result<bool> {
    file.rewind()?;
    let mut offset = 0;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(offset == expected.len());
        }
        let Some(end) = offset.checked_add(read) else {
            return Ok(false);
        };
        if expected.get(offset..end) != Some(&buffer[..read]) {
            return Ok(false);
        }
        offset = end;
    }
}

fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if file.metadata()?.file_type().is_file() {
        Ok(file)
    } else {
        Err(io::Error::other("target is not a regular file"))
    }
}

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> io::Result<FileIdentity> {
    Ok(FileIdentity)
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
fn publish_new_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::hard_link(source, target)?;
    fs::remove_file(source)
}

#[cfg(windows)]
fn publish_new_file(source: &Path, target: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source = wide_path(source);
    let target = wide_path(target);
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn publish_stage(
    source: &Path,
    target: &Path,
    existing: Option<&ObservedTarget>,
    stage_cleanup: &mut StageCleanup<'_>,
) -> Result<()> {
    let Some(existing) = existing else {
        publish_new_file(source, target)?;
        return Ok(());
    };
    replace_existing_file(source, target, existing, stage_cleanup)
}

#[cfg(target_os = "linux")]
fn exchange_files(left: &Path, right: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn exchange_files(left: &Path, right: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn replace_existing_file(
    source: &Path,
    target: &Path,
    existing: &ObservedTarget,
    stage_cleanup: &mut StageCleanup<'_>,
) -> Result<()> {
    exchange_files(source, target)?;
    if let Err(changed) = verify_target_unchanged(source, existing) {
        if let Err(rollback) = exchange_files(target, source) {
            // `source` contains the displaced external file. Never let the
            // ordinary stage cleanup destroy the only recoverable copy.
            stage_cleanup.disarm();
            return Err(anyhow!(
                "{changed:#}; failed to restore concurrently changed target; displaced file remains at {}: {rollback}",
                source.display()
            ));
        }
        return Err(changed);
    }
    fs::remove_file(source)
        .with_context(|| format!("remove displaced file {}", source.display()))?;
    Ok(())
}

#[cfg(windows)]
fn replace_existing_file(
    source: &Path,
    target: &Path,
    existing: &ObservedTarget,
    _stage_cleanup: &mut StageCleanup<'_>,
) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    let backup = stage_path(target)?;
    let source_wide = wide_path(source);
    let target_wide = wide_path(target);
    let backup_wide = wide_path(&backup);
    if unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            source_wide.as_ptr(),
            backup_wide.as_ptr(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }

    if let Err(changed) = verify_target_unchanged(&backup, existing) {
        if unsafe {
            ReplaceFileW(
                target_wide.as_ptr(),
                backup_wide.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(anyhow!(
                "{changed:#}; failed to restore concurrently changed target; displaced file remains at {}: {}",
                backup.display(),
                io::Error::last_os_error()
            ));
        }
        return Err(changed);
    }
    fs::remove_file(&backup)
        .with_context(|| format!("remove displaced file {}", backup.display()))?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn replace_existing_file(
    source: &Path,
    target: &Path,
    existing: &ObservedTarget,
    _stage_cleanup: &mut StageCleanup<'_>,
) -> Result<()> {
    // These targets lack an atomic path-exchange primitive in libc. Retain the
    // identity/content check and existing atomic-rename behavior, while the
    // exchange-capable platforms additionally close the final path-swap race.
    verify_target_unchanged(target, existing)?;
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(not(test))]
fn run_before_publish_hook(_path: &Path) {}

#[cfg(test)]
fn run_before_publish_hook(path: &Path) {
    BEFORE_PUBLISH_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook(path);
        }
    });
}

#[cfg(test)]
type BeforePublishHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    static BEFORE_PUBLISH_HOOK: std::cell::RefCell<Option<BeforePublishHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn with_before_publish_hook<T>(
    hook: impl FnOnce(&Path) + 'static,
    update: impl FnOnce() -> T,
) -> T {
    BEFORE_PUBLISH_HOOK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let result = update();
    BEFORE_PUBLISH_HOOK.with(|slot| {
        slot.borrow_mut().take();
    });
    result
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

    #[test]
    fn concurrent_content_edit_is_detected_without_losing_external_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        fs::write(&path, b"original").unwrap();
        let external_path = path.clone();

        let result = with_before_publish_hook(
            move |_| fs::write(&external_path, b"external edit").unwrap(),
            || atomic_update(&path, |_| Ok(b"ctx replacement".to_vec())),
        );

        assert!(format!("{:#}", result.unwrap_err()).contains("concurrently changed"));
        assert_eq!(fs::read(&path).unwrap(), b"external edit");
    }

    #[test]
    fn concurrent_path_swap_is_detected_without_overwriting_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        let displaced = root.path().join("displaced.json");
        fs::write(&path, b"original").unwrap();
        let external_path = path.clone();
        let external_displaced = displaced.clone();

        let result = with_before_publish_hook(
            move |_| {
                fs::rename(&external_path, &external_displaced).unwrap();
                fs::write(&external_path, b"external replacement").unwrap();
            },
            || atomic_update(&path, |_| Ok(b"ctx replacement".to_vec())),
        );

        assert!(format!("{:#}", result.unwrap_err()).contains("concurrently changed"));
        assert_eq!(fs::read(&path).unwrap(), b"external replacement");
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
    }

    #[test]
    fn concurrent_creation_wins_over_missing_target_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        let external_path = path.clone();

        let result = with_before_publish_hook(
            move |_| fs::write(&external_path, b"external creation").unwrap(),
            || atomic_update(&path, |_| Ok(b"ctx replacement".to_vec())),
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external creation");
    }
}
