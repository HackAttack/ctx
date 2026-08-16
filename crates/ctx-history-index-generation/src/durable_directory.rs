//! A Tantivy directory whose atomic publications include the durability barrier
//! required before Tantivy may garbage-collect the previous generation.

use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use tantivy::directory::{
    error::{DeleteError, LockError, OpenDirectoryError, OpenReadError, OpenWriteError},
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, MmapDirectory, WatchCallback,
    WatchHandle, WritePtr,
};
use tantivy::HasLen;
use uuid::Uuid;

const TEMPORARY_FILE_PREFIX: &str = ".ctx-tantivy-atomic-";
const TEMPORARY_FILE_ATTEMPTS: usize = 16;

/// An [`MmapDirectory`] that does not return from `atomic_write` until the
/// replacement is durable.
///
/// Tantivy publishes `meta.json` and then immediately becomes free to garbage
/// collect files from the previous generation. `MmapDirectory` synchronizes
/// the temporary file before replacing `meta.json`, but on Unix its atomic
/// write does not synchronize the containing directory after the replacement.
/// This wrapper owns that final barrier.
///
/// A failure from the final directory synchronization happens after the
/// replacement became visible. Returning that error is intentional: reporting
/// success would claim a durability guarantee that the filesystem did not
/// provide. Higher-level publication code must reconcile the visible target;
/// predecessor migration exposes that case as a committed recovery outcome.
#[derive(Clone)]
pub struct DurableMmapDirectory {
    inner: DurableDirectoryBackend,
    root_path: Arc<PathBuf>,
}

#[derive(Clone)]
enum DurableDirectoryBackend {
    Mmap(MmapDirectory),
    Anchored(Arc<crate::read_root::OpenedDirectory>),
}

#[derive(Debug)]
pub enum DurableAtomicWriteOutcome {
    Durable,
    VisibleButDurabilityUncertain(io::Error),
}

impl DurableAtomicWriteOutcome {
    fn into_io_result(self) -> io::Result<()> {
        match self {
            Self::Durable => Ok(()),
            Self::VisibleButDurabilityUncertain(error) => Err(error),
        }
    }
}

impl DurableMmapDirectory {
    pub fn open(directory_path: impl AsRef<Path>) -> Result<Self, OpenDirectoryError> {
        let directory_path = directory_path.as_ref();
        if let Some(opened) =
            crate::read_root::registered_read_directory(directory_path).map_err(|error| {
                OpenDirectoryError::wrap_io_error(error, directory_path.to_path_buf())
            })?
        {
            return Ok(Self {
                inner: DurableDirectoryBackend::Anchored(opened),
                root_path: Arc::new(directory_path.to_path_buf()),
            });
        }
        let inner = DurableDirectoryBackend::Mmap(MmapDirectory::open(directory_path)?);
        let root_path = canonical_root_path(directory_path)?;
        Ok(Self {
            inner,
            root_path: Arc::new(root_path),
        })
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn atomic_write_with_outcome(
        &self,
        path: &Path,
        data: &[u8],
    ) -> io::Result<DurableAtomicWriteOutcome> {
        if matches!(&self.inner, DurableDirectoryBackend::Anchored(_)) {
            return Err(read_only_directory_error());
        }
        match self.atomic_write_with_outcome_validated(path, data, || Ok(())) {
            Ok(outcome) => Ok(outcome),
            Err(crate::GenerationError::Io(error)) => Err(error),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub(crate) fn atomic_write_with_outcome_validated<F>(
        &self,
        path: &Path,
        data: &[u8],
        validate_before_replace: F,
    ) -> crate::Result<DurableAtomicWriteOutcome>
    where
        F: FnOnce() -> crate::Result<()>,
    {
        let target_path = self.resolve_path(path);
        let parent_path = target_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no parent directory", target_path.display()),
            )
        })?;
        // Open the synchronization handle before publication. Any later error
        // is therefore known to occur either before replacement or after the
        // target became visible.
        let parent_sync = ParentDirectorySync::open(parent_path)?;
        atomic_replace_with_outcome_validated(
            &target_path,
            data,
            replace_file,
            move || parent_sync.sync(),
            validate_before_replace,
        )
    }

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.root_path.join(relative_path)
    }
}

pub fn reclaim_abandoned_atomic_writes(directory_path: &Path) -> io::Result<()> {
    let mut removed_file = false;
    for entry in fs::read_dir(directory_path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !is_atomic_temporary_file(&entry.file_name()) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed_file = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if removed_file {
        ParentDirectorySync::open(directory_path)?.sync()?;
    }
    Ok(())
}

/// Atomically replaces `target` with an already-synchronized staged file.
///
/// Both paths must have the same parent so the operation cannot degrade into a
/// cross-filesystem copy. The published file and its directory entry are
/// synchronized before this function returns. Windows uses `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`; directory flushing is
/// intentionally skipped there because it is not a reliable Windows durability
/// primitive.
pub fn durable_atomic_replace_file(source: &Path, target: &Path) -> io::Result<()> {
    let source_parent = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source path {} has no parent directory", source.display()),
        )
    })?;
    let target_parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("target path {} has no parent directory", target.display()),
        )
    })?;
    if source_parent != target_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic replacement requires one directory: {} and {} differ",
                source_parent.display(),
                target_parent.display()
            ),
        ));
    }

    // Acquire the directory synchronization handle before publication so a
    // failure to open it cannot occur after the target becomes visible.
    let parent_sync = ParentDirectorySync::open(target_parent)?;
    replace_file(source, target)?;
    File::open(target)?.sync_all()?;
    parent_sync.sync()
}

fn is_atomic_temporary_file(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(identifier) = name
        .strip_prefix(TEMPORARY_FILE_PREFIX)
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    identifier.len() == 32
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl fmt::Debug for DurableMmapDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DurableMmapDirectory")
            .field(&self.root_path)
            .finish()
    }
}

impl Directory for DurableMmapDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.get_file_handle(path),
            DurableDirectoryBackend::Anchored(inner) => {
                let file = inner.open_file(path).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        OpenReadError::FileDoesNotExist(path.to_path_buf())
                    } else {
                        OpenReadError::wrap_io_error(error, path.to_path_buf())
                    }
                })?;
                AnchoredFileHandle::new(file)
                    .map(|handle| Arc::new(handle) as Arc<dyn FileHandle>)
                    .map_err(|error| OpenReadError::wrap_io_error(error, path.to_path_buf()))
            }
        }
    }

    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.open_read(path),
            DurableDirectoryBackend::Anchored(_) => self.get_file_handle(path).map(FileSlice::new),
        }
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.delete(path),
            DurableDirectoryBackend::Anchored(_) => Err(DeleteError::IoError {
                io_error: Arc::new(read_only_directory_error()),
                filepath: path.to_path_buf(),
            }),
        }
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.exists(path),
            DurableDirectoryBackend::Anchored(inner) => match inner.open_file(path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(OpenReadError::wrap_io_error(error, path.to_path_buf())),
            },
        }
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.open_write(path),
            DurableDirectoryBackend::Anchored(_) => Err(OpenWriteError::wrap_io_error(
                read_only_directory_error(),
                path.to_path_buf(),
            )),
        }
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.atomic_read(path),
            DurableDirectoryBackend::Anchored(inner) => {
                let mut file = inner.open_file(path).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        OpenReadError::FileDoesNotExist(path.to_path_buf())
                    } else {
                        OpenReadError::wrap_io_error(error, path.to_path_buf())
                    }
                })?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .map_err(|error| OpenReadError::wrap_io_error(error, path.to_path_buf()))?;
                Ok(bytes)
            }
        }
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.atomic_write_with_outcome(path, data)?.into_io_result()
    }

    fn sync_directory(&self) -> io::Result<()> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.sync_directory(),
            DurableDirectoryBackend::Anchored(inner) => inner.sync(),
        }
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.acquire_lock(lock),
            // Immutable generations are protected from outer reclamation by
            // GenerationReadLease. Tantivy's meta lock only coordinates its
            // own mutable-directory GC, which cannot run through this
            // read-only capability.
            DurableDirectoryBackend::Anchored(_) => {
                let _ = lock;
                Ok(Box::new(()).into())
            }
        }
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.watch(watch_callback),
            DurableDirectoryBackend::Anchored(_) => {
                let _ = watch_callback;
                Ok(WatchHandle::empty())
            }
        }
    }
}

#[derive(Debug)]
struct AnchoredFileHandle {
    file: File,
    len: usize,
}

impl AnchoredFileHandle {
    fn new(file: File) -> io::Result<Self> {
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::other("anchored generation file is too large"))?;
        Ok(Self { file, len })
    }
}

impl HasLen for AnchoredFileHandle {
    fn len(&self) -> usize {
        self.len
    }
}

impl FileHandle for AnchoredFileHandle {
    fn read_bytes(
        &self,
        range: std::ops::Range<usize>,
    ) -> io::Result<tantivy::directory::OwnedBytes> {
        if range.start > range.end || range.end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anchored generation read is outside the file",
            ));
        }
        let mut bytes = vec![0_u8; range.len()];
        #[cfg(unix)]
        std::os::unix::fs::FileExt::read_exact_at(&self.file, &mut bytes, range.start as u64)?;
        #[cfg(windows)]
        {
            let mut read = 0_usize;
            while read < bytes.len() {
                let count = std::os::windows::fs::FileExt::seek_read(
                    &self.file,
                    &mut bytes[read..],
                    (range.start + read) as u64,
                )?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "anchored generation read",
                    ));
                }
                read += count;
            }
        }
        Ok(tantivy::directory::OwnedBytes::new(bytes))
    }
}

fn read_only_directory_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "anchored generation directories are read-only",
    )
}

fn canonical_root_path(directory_path: &Path) -> Result<PathBuf, OpenDirectoryError> {
    match directory_path.canonicalize() {
        Ok(canonical_path) => Ok(canonical_path),
        Err(io_error) => {
            // Match MmapDirectory's public Windows behavior for virtual drives,
            // where canonicalize can fail with ERROR_INVALID_FUNCTION even
            // though the directory was successfully opened.
            #[cfg(windows)]
            if io_error.raw_os_error() == Some(1) && directory_path.exists() {
                return Ok(directory_path.to_path_buf());
            }
            Err(OpenDirectoryError::wrap_io_error(
                io_error,
                directory_path.to_path_buf(),
            ))
        }
    }
}

#[cfg(test)]
fn atomic_replace_with<Replace, SyncParent>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
) -> io::Result<()>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
    SyncParent: FnOnce() -> io::Result<()>,
{
    atomic_replace_with_outcome(target_path, data, replace, sync_parent)?.into_io_result()
}

#[cfg(test)]
fn atomic_replace_with_outcome<Replace, SyncParent>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
) -> io::Result<DurableAtomicWriteOutcome>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
    SyncParent: FnOnce() -> io::Result<()>,
{
    match atomic_replace_with_outcome_validated(target_path, data, replace, sync_parent, || Ok(()))
    {
        Ok(outcome) => Ok(outcome),
        Err(crate::GenerationError::Io(error)) => Err(error),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn atomic_replace_with_outcome_validated<Replace, SyncParent, Validate>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
    validate_before_replace: Validate,
) -> crate::Result<DurableAtomicWriteOutcome>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
    SyncParent: FnOnce() -> io::Result<()>,
    Validate: FnOnce() -> crate::Result<()>,
{
    let parent_path = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", target_path.display()),
        )
    })?;
    let (temporary_path, mut temporary_file) = create_temporary_file(parent_path)?;

    atomic_write_checkpoint(AtomicWriteStage::BeforeTemporaryWrite, target_path)?;

    let write_result = temporary_file
        .write_all(data)
        .and_then(|()| temporary_file.flush())
        .and_then(|()| temporary_file.sync_all());
    // Windows does not permit moving this file while its default, non-sharing
    // handle is open.
    drop(temporary_file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }

    atomic_write_checkpoint(
        AtomicWriteStage::AfterTemporarySyncBeforeReplace,
        target_path,
    )?;
    atomic_write_checkpoint(AtomicWriteStage::BeforeReplace, target_path)?;

    // This is the terminal publication fence: every fallible preparation and
    // test checkpoint has completed, and the replacement below is the next
    // operation. The validator can therefore reject a raced candidate while
    // the previous target is still authoritative.
    if let Err(error) = validate_before_replace() {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    if let Err(error) = replace(&temporary_path, target_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }

    if let Err(error) = atomic_write_checkpoint(
        AtomicWriteStage::AfterReplaceBeforeDirectorySync,
        target_path,
    ) {
        return Ok(DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(
            error,
        ));
    }

    match sync_parent() {
        Ok(()) => Ok(DurableAtomicWriteOutcome::Durable),
        Err(error) => Ok(DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(
            error,
        )),
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy)]
enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(any(test, feature = "test-support"))]
type AtomicWriteTestHook = Box<dyn for<'a> FnMut(AtomicWriteStage, &'a Path) -> io::Result<()>>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static ATOMIC_WRITE_TEST_HOOK: std::cell::RefCell<Option<AtomicWriteTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(any(test, feature = "test-support"))]
pub struct AtomicWriteTestHookGuard(Option<AtomicWriteTestHook>);

#[cfg(any(test, feature = "test-support"))]
impl AtomicWriteTestHookGuard {
    pub fn set<F>(hook: F) -> Self
    where
        F: for<'a> FnMut(AtomicWriteStage, &'a Path) -> io::Result<()> + 'static,
    {
        let previous = ATOMIC_WRITE_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for AtomicWriteTestHookGuard {
    fn drop(&mut self) {
        ATOMIC_WRITE_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn atomic_write_checkpoint(stage: AtomicWriteStage, target: &Path) -> io::Result<()> {
    ATOMIC_WRITE_TEST_HOOK.with(|active| {
        let mut active = active.borrow_mut();
        match active.as_mut() {
            Some(hook) => hook(stage, target),
            None => Ok(()),
        }
    })
}

#[cfg(not(any(test, feature = "test-support")))]
fn atomic_write_checkpoint(_stage: AtomicWriteStage, _target: &Path) -> io::Result<()> {
    Ok(())
}

fn create_temporary_file(parent_path: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let temporary_path = parent_path.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}.tmp",
            Uuid::new_v4().simple()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique Tantivy atomic-write file",
    ))
}

#[cfg(not(windows))]
struct ParentDirectorySync(File);

#[cfg(not(windows))]
impl ParentDirectorySync {
    fn open(parent_path: &Path) -> io::Result<Self> {
        File::open(parent_path).map(Self)
    }

    fn sync(self) -> io::Result<()> {
        self.0.sync_all()
    }
}

#[cfg(windows)]
struct ParentDirectorySync;

#[cfg(windows)]
impl ParentDirectorySync {
    fn open(_parent_path: &Path) -> io::Result<Self> {
        Ok(Self)
    }

    fn sync(self) -> io::Result<()> {
        // MoveFileExW with MOVEFILE_WRITE_THROUGH, used below, does not
        // return until the move has reached disk. Opening and flushing a
        // directory handle is not a reliable substitute on Windows: it is a
        // no-op on local disks and can fail on virtual drives.
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "MoveFileExW"]
        fn move_file_ex_w(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn nul_terminated(path: &Path) -> io::Result<Vec<u16>> {
        let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if path_wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows path contains an interior NUL",
            ));
        }
        path_wide.push(0);
        Ok(path_wide)
    }

    let source_wide = nul_terminated(source)?;
    let target_wide = nul_terminated(target)?;
    // SAFETY: both path buffers are NUL-terminated and remain alive for the
    // duration of the call.
    let moved = unsafe {
        move_file_ex_w(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn atomic_write_replaces_existing_file() {
        let temporary_directory = tempdir().unwrap();
        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let path = Path::new("meta.json");

        directory.atomic_write(path, b"previous").unwrap();
        directory.atomic_write(path, b"replacement").unwrap();

        assert_eq!(directory.atomic_read(path).unwrap(), b"replacement");
        assert_no_temporary_files(temporary_directory.path());
    }

    #[test]
    fn durable_staged_file_replacement_supports_first_publish_and_replace() {
        let temporary_directory = tempdir().unwrap();
        let target = temporary_directory.path().join("projection.sqlite");
        let first = temporary_directory.path().join("projection.first");
        fs::write(&first, b"first").unwrap();
        File::open(&first).unwrap().sync_all().unwrap();

        durable_atomic_replace_file(&first, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert!(!first.exists());

        let replacement = temporary_directory.path().join("projection.replacement");
        fs::write(&replacement, b"replacement").unwrap();
        File::open(&replacement).unwrap().sync_all().unwrap();

        durable_atomic_replace_file(&replacement, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert!(!replacement.exists());
    }

    #[test]
    fn replacement_failure_preserves_previous_file_and_removes_temporary_file() {
        let temporary_directory = tempdir().unwrap();
        let target_path = temporary_directory.path().join("meta.json");
        fs::write(&target_path, b"previous").unwrap();

        let error = atomic_replace_with(
            &target_path,
            b"replacement",
            |temporary_path, target_path| {
                assert_eq!(fs::read(temporary_path).unwrap(), b"replacement");
                assert_eq!(fs::read(target_path).unwrap(), b"previous");
                Err(io::Error::other("injected replacement failure"))
            },
            || panic!("parent sync must not run after replacement failure"),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "injected replacement failure");
        assert_eq!(fs::read(&target_path).unwrap(), b"previous");
        assert_no_temporary_files(temporary_directory.path());
    }

    #[test]
    fn parent_sync_failure_is_reported_after_replacement_is_visible() {
        let temporary_directory = tempdir().unwrap();
        let target_path = temporary_directory.path().join("meta.json");
        fs::write(&target_path, b"previous").unwrap();
        let sync_attempted = AtomicBool::new(false);

        let error = atomic_replace_with(&target_path, b"replacement", replace_file, || {
            sync_attempted.store(true, Ordering::SeqCst);
            Err(io::Error::other("injected parent sync failure"))
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "injected parent sync failure");
        assert!(sync_attempted.load(Ordering::SeqCst));
        assert_eq!(fs::read(&target_path).unwrap(), b"replacement");
        assert_no_temporary_files(temporary_directory.path());
    }

    #[test]
    fn abandoned_atomic_write_reclamation_is_limited_to_owned_regular_files() {
        let temporary_directory = tempdir().unwrap();
        let owned = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp");
        let near_miss = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp.keep");
        let foreign = temporary_directory.path().join("foreign.tmp");
        let matching_directory = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-fedcba9876543210fedcba9876543210.tmp");
        fs::write(&owned, b"abandoned").unwrap();
        fs::write(&near_miss, b"preserve").unwrap();
        fs::write(&foreign, b"preserve").unwrap();
        fs::create_dir(&matching_directory).unwrap();

        reclaim_abandoned_atomic_writes(temporary_directory.path()).unwrap();

        assert!(!owned.exists());
        assert!(near_miss.is_file());
        assert!(foreign.is_file());
        assert!(matching_directory.is_dir());
    }

    fn assert_no_temporary_files(directory: &Path) {
        let temporary_files = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(TEMPORARY_FILE_PREFIX))
            .collect::<Vec<_>>();
        assert!(temporary_files.is_empty(), "{temporary_files:?}");
    }
}
