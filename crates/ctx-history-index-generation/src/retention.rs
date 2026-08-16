use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use ctx_history_platform::platform_security::{restrict_private_file_handle, verify_private_file};
use serde::{Deserialize, Serialize};
use tantivy::directory::Lock;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

use crate::lock::{acquire_generation_ownership_fence_in, GenerationOwnershipFence};
use crate::{
    acquire_generation_writer_lock_with_retry, durable_atomic_replace_file, is_generation_id,
    load_active_generation_pointer, slot_path, sync_directory, DurableMmapDirectory,
    GenerationError as IndexError, GenerationSlot, Result, GENERATION_WRITER_LOCK_FILE,
};

const GENERATION_RETENTION_LEASE_FILE: &str = "generation-retention-lease.json";
const GENERATION_RETENTION_LEASE_STAGED_FILE: &str = ".generation-retention-lease.next";
const GENERATION_RETENTION_LEASE_VERSION: u16 = 1;
const MAX_GENERATION_RETENTION_LEASE_BYTES: u64 = 4 * 1024;
const MAX_GENERATION_RETENTION_OWNER_KIND_BYTES: usize = 64;
const GENERATION_READ_LEASE_PREFIX: &str = ".ctx-generation-read-lease-v1-";
const GENERATION_READ_LEASE_SUFFIX: &str = ".lock";
const MAX_GENERATION_READ_LEASE_FILES: usize = 4_096;

/// A process-scoped hold on one exact immutable generation.
///
/// The empty marker survives a crash, while its shared OS lock does not. A
/// later writer can therefore distinguish a live reader from stale state and
/// reclaim the marker together with the generation it used to protect.
#[derive(Debug)]
pub struct GenerationReadLease {
    root: PathBuf,
    target: GenerationSlot,
    file: File,
}

impl GenerationReadLease {
    pub fn generation_id(&self) -> &str {
        self.target.generation_id()
    }

    #[doc(hidden)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[doc(hidden)]
    pub fn target(&self) -> &GenerationSlot {
        &self.target
    }
}

impl Drop for GenerationReadLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// A read lease whose short publication fence is still held.
///
/// Callers perform the bounded pointer-bound certification check while this
/// value is alive, then release only the ownership fence. The returned shared
/// lease remains live while manifest and searcher construction continues.
pub struct GenerationReadLeaseAcquisition {
    lease: GenerationReadLease,
    pointer: crate::ActiveGenerationPointer,
    _ownership_fence: GenerationOwnershipFence,
}

impl GenerationReadLeaseAcquisition {
    pub fn root(&self) -> &Path {
        self.lease.root()
    }

    pub fn pointer(&self) -> &crate::ActiveGenerationPointer {
        &self.pointer
    }

    pub fn target(&self) -> &GenerationSlot {
        self.lease.target()
    }

    pub fn release_publication_fence(self) -> GenerationReadLease {
        let Self {
            lease,
            pointer: _,
            _ownership_fence,
        } = self;
        drop(_ownership_fence);
        lease
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationReadLeaseTarget {
    generation_id: String,
    directory: String,
}

impl GenerationReadLeaseTarget {
    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn directory(&self) -> &str {
        &self.directory
    }
}

/// The sole bounded durable hold on an immutable generation outside the
/// active/previous publication pair.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRetentionLease {
    version: u16,
    owner_kind: String,
    owner_id: String,
    target: GenerationSlot,
}

impl GenerationRetentionLease {
    pub fn owner_kind(&self) -> &str {
        &self.owner_kind
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn generation_id(&self) -> &str {
        self.target.generation_id()
    }

    #[doc(hidden)]
    pub fn target(&self) -> &GenerationSlot {
        &self.target
    }

    fn validate(&self) -> Result<()> {
        if self.version != GENERATION_RETENTION_LEASE_VERSION {
            return Err(IndexError::UnsupportedGenerationRetentionLease(u32::from(
                self.version,
            )));
        }
        if self.owner_kind.is_empty()
            || self.owner_kind.len() > MAX_GENERATION_RETENTION_OWNER_KIND_BYTES
            || !self
                .owner_kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !is_generation_id(&self.owner_id)
        {
            return Err(IndexError::InvalidGenerationRetentionLeaseOwner);
        }
        self.target.validate()
    }
}

/// Atomically acquires the one durable lease while serialized with Core
/// publication and reclamation. Exact replay by the same owner is idempotent.
pub fn acquire_generation_retention_lease(
    root: impl AsRef<Path>,
    generation_id: &str,
    owner_kind: &str,
    owner_id: &str,
) -> Result<GenerationRetentionLease> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let root = canonical_index_root(root.as_ref())?;
    let directory = DurableMmapDirectory::open(&root).map_err(tantivy::TantivyError::from)?;
    let lock = Lock {
        filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
        is_blocking: false,
    };
    let _publication_lock = acquire_generation_writer_lock_with_retry(&directory, &lock)?;

    if let Some(existing) = load_generation_retention_lease(&root)? {
        if existing.generation_id() == generation_id
            && existing.owner_kind == owner_kind
            && existing.owner_id == owner_id
        {
            return Ok(existing);
        }
        return Err(IndexError::GenerationRetentionLeaseConflict {
            retained_generation_id: existing.generation_id().to_owned(),
            owner_kind: existing.owner_kind,
        });
    }

    let pointer =
        load_active_generation_pointer(&root)?.ok_or(IndexError::MissingActiveGenerationPointer)?;
    let target = std::iter::once(pointer.active())
        .chain(pointer.previous())
        .find(|slot| slot.generation_id() == generation_id)
        .cloned()
        .ok_or_else(|| IndexError::GenerationRetentionLeaseTargetNotRetained {
            requested_generation_id: generation_id.to_owned(),
        })?;
    let lease = GenerationRetentionLease {
        version: GENERATION_RETENTION_LEASE_VERSION,
        owner_kind: owner_kind.to_owned(),
        owner_id: owner_id.to_owned(),
        target,
    };
    lease.validate()?;
    publish_lease(&root, &lease)?;
    Ok(lease)
}

/// Selects and leases the active generation while serialized with publication
/// and reclamation. The publication fence remains held in the returned value
/// only long enough for the caller's bounded physical-certification check.
pub fn acquire_active_generation_read_lease(
    root: impl AsRef<Path>,
) -> Result<GenerationReadLeaseAcquisition> {
    acquire_generation_read_lease_inner(root.as_ref(), None)
}

/// Leases exactly one currently retained generation while serialized with
/// publication and reclamation. No unrelated-generation fallback is allowed.
pub fn acquire_generation_read_lease(
    root: impl AsRef<Path>,
    generation_id: &str,
) -> Result<GenerationReadLeaseAcquisition> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    acquire_generation_read_lease_inner(root.as_ref(), Some(generation_id))
}

fn acquire_generation_read_lease_inner(
    root: &Path,
    generation_id: Option<&str>,
) -> Result<GenerationReadLeaseAcquisition> {
    let root = canonical_index_root(root)?;
    let directory = DurableMmapDirectory::open(&root).map_err(tantivy::TantivyError::from)?;
    let ownership_fence = acquire_generation_ownership_fence_in(&directory)?;
    let pointer =
        load_active_generation_pointer(&root)?.ok_or(IndexError::MissingActiveGenerationPointer)?;
    let target = match generation_id {
        None => pointer.active().clone(),
        Some(generation_id) => std::iter::once(pointer.active())
            .chain(pointer.previous())
            .chain(
                load_generation_retention_lease(&root)?
                    .as_ref()
                    .map(GenerationRetentionLease::target),
            )
            .find(|slot| slot.generation_id() == generation_id)
            .cloned()
            .ok_or_else(|| IndexError::GenerationRetentionLeaseTargetNotRetained {
                requested_generation_id: generation_id.to_owned(),
            })?,
    };
    let path = generation_read_lease_path(&root, &target);
    let file = open_generation_read_lease_file(&path)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(GenerationReadLeaseAcquisition {
            lease: GenerationReadLease { root, target, file },
            pointer,
            _ownership_fence: ownership_fence,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Err(IndexError::GenerationRetentionLeaseConflict {
                retained_generation_id: target.generation_id().to_owned(),
                owner_kind: "generation_reader".to_owned(),
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Returns live reader targets and removes stale crash markers.
///
/// Reclamation calls this while holding the generation ownership fence, so a
/// new reader cannot appear between this scan and deletion of unretained state.
pub(crate) fn live_generation_read_lease_targets(
    root: &Path,
) -> Result<Vec<GenerationReadLeaseTarget>> {
    let mut targets = Vec::new();
    let mut matching_entries = 0_usize;
    let mut removed = false;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !file_name.starts_with(GENERATION_READ_LEASE_PREFIX) {
            continue;
        }
        matching_entries = matching_entries
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        if matching_entries > MAX_GENERATION_READ_LEASE_FILES {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }
        let target = parse_generation_read_lease_file_name(&file_name)
            .ok_or(IndexError::InvalidGenerationRetentionLease)?;
        if !entry.file_type()?.is_file() {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }
        let file = open_existing_generation_read_lease_file(&entry.path())?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                fs2::FileExt::unlock(&file)?;
                drop(file);
                fs::remove_file(entry.path())?;
                removed = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => targets.push(target),
            Err(error) => return Err(error.into()),
        }
    }
    if removed {
        sync_directory(root)?;
    }
    Ok(targets)
}

/// Loads and strictly validates the sole lease. Corrupt, oversized, or
/// non-private state is actionable and never broadens retention.
pub fn load_generation_retention_lease(
    root: impl AsRef<Path>,
) -> Result<Option<GenerationRetentionLease>> {
    let path = lease_path(root.as_ref());
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_GENERATION_RETENTION_LEASE_BYTES
        || verify_private_file(&path).is_err()
    {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(&path)?
        .take(MAX_GENERATION_RETENTION_LEASE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_GENERATION_RETENTION_LEASE_BYTES {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let lease: GenerationRetentionLease =
        serde_json::from_slice(&bytes).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if serde_json::to_vec(&lease)? != bytes {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    lease.validate()?;
    let target = slot_path(root.as_ref(), lease.target());
    let target_metadata =
        fs::symlink_metadata(target).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    Ok(Some(lease))
}

/// Releases exactly the observed owner under the publication lock. The next
/// writer open/publication performs ordinary bounded reclamation.
pub fn release_generation_retention_lease(
    root: impl AsRef<Path>,
    expected: &GenerationRetentionLease,
) -> Result<bool> {
    let root = match canonical_existing_index_root(root.as_ref())? {
        Some(root) => root,
        None => return Ok(false),
    };
    let directory = DurableMmapDirectory::open(&root).map_err(tantivy::TantivyError::from)?;
    let lock = Lock {
        filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
        is_blocking: false,
    };
    let _publication_lock = acquire_generation_writer_lock_with_retry(&directory, &lock)?;
    let Some(current) = load_generation_retention_lease(&root)? else {
        return Ok(false);
    };
    if &current != expected {
        return Err(IndexError::GenerationRetentionLeaseOwnerMismatch);
    }
    remove_lease_file(&root)?;
    Ok(true)
}

fn canonical_index_root(root: &Path) -> Result<PathBuf> {
    if !root.is_dir() {
        return Err(IndexError::MissingActiveGenerationPointer);
    }
    canonical_existing_index_root(root)?.ok_or(IndexError::MissingActiveGenerationPointer)
}

fn canonical_existing_index_root(root: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(root) {
        Ok(root) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn lease_path(root: &Path) -> PathBuf {
    root.join(GENERATION_RETENTION_LEASE_FILE)
}

fn generation_read_lease_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(format!(
        "{GENERATION_READ_LEASE_PREFIX}{}.{}{GENERATION_READ_LEASE_SUFFIX}",
        slot.generation_id(),
        slot.directory()
    ))
}

fn parse_generation_read_lease_file_name(file_name: &str) -> Option<GenerationReadLeaseTarget> {
    let body = file_name
        .strip_prefix(GENERATION_READ_LEASE_PREFIX)?
        .strip_suffix(GENERATION_READ_LEASE_SUFFIX)?;
    let (generation_id, directory) = body.split_once('.')?;
    if body.matches('.').count() != 1 || !GenerationSlot::names_are_valid(generation_id, directory)
    {
        return None;
    }
    Some(GenerationReadLeaseTarget {
        generation_id: generation_id.to_owned(),
        directory: directory.to_owned(),
    })
}

fn open_generation_read_lease_file(path: &Path) -> Result<File> {
    let mut options = generation_read_lease_open_options();
    match options.create_new(true).open(path) {
        Ok(file) => {
            restrict_private_file_handle(&file)?;
            file.sync_all()?;
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_generation_read_lease_file(path)
        }
        Err(error) => Err(error.into()),
    }
}

fn open_existing_generation_read_lease_file(path: &Path) -> Result<File> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != 0
        || verify_private_file(path).is_err()
    {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let file = generation_read_lease_open_options()
        .open(path)
        .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    let opened = file
        .metadata()
        .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if !opened.is_file() || opened.len() != 0 {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    Ok(file)
}

fn generation_read_lease_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    #[cfg(windows)]
    options
        .share_mode(
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
        )
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

fn publish_lease(root: &Path, lease: &GenerationRetentionLease) -> Result<()> {
    let bytes = serde_json::to_vec(lease)?;
    let staged = root.join(GENERATION_RETENTION_LEASE_STAGED_FILE);
    match fs::remove_file(&staged) {
        Ok(()) => sync_directory(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    restrict_private_file_handle(&file)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let target = lease_path(root);
    if let Err(error) = durable_atomic_replace_file(&staged, &target) {
        let _ = fs::remove_file(&staged);
        return Err(error.into());
    }
    if verify_private_file(&target).is_err() {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    Ok(())
}

fn remove_lease_file(root: &Path) -> Result<()> {
    match fs::remove_file(lease_path(root)) {
        Ok(()) => sync_directory(root).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, sync::mpsc, thread, time::Duration};

    use tempfile::tempdir;

    use super::*;
    use crate::certification::certification_path;
    use crate::{
        manifest_path, publish_active_generation_pointer, reclaim_inactive_generation_directories,
        reclaim_unreferenced_certifications, reclaim_unreferenced_manifests, sha256_hex,
        write_manifest_bytes, ActiveGenerationPointer, INDEX_GENERATIONS_DIRECTORY,
    };

    const CHILD_ROOT: &str = "CTX_GENERATION_READ_LEASE_CHILD_ROOT";
    const CHILD_GENERATION: &str = "CTX_GENERATION_READ_LEASE_CHILD_GENERATION";
    const CHILD_MARKER: &str = "CTX_GENERATION_READ_LEASE_CHILD_MARKER";

    fn create_slot(root: &Path, digit: char) -> GenerationSlot {
        let bytes = format!(r#"{{"generation":"{digit}"}}"#).into_bytes();
        let generation_id = sha256_hex(&bytes);
        let directory = format!("generation-{}", digit.to_string().repeat(32));
        fs::create_dir_all(root.join(INDEX_GENERATIONS_DIRECTORY).join(&directory)).unwrap();
        write_manifest_bytes(root, &generation_id, &bytes).unwrap();
        let slot = GenerationSlot::new(
            generation_id,
            directory,
            sha256_hex(format!("physical-{digit}").as_bytes()),
        )
        .unwrap();
        let certification = certification_path(root, &slot);
        fs::create_dir_all(certification.parent().unwrap()).unwrap();
        fs::write(certification, b"test-certification").unwrap();
        slot
    }

    #[test]
    fn generation_read_lease_crash_child() {
        let (Ok(root), Ok(generation_id), Ok(marker)) = (
            std::env::var(CHILD_ROOT),
            std::env::var(CHILD_GENERATION),
            std::env::var(CHILD_MARKER),
        ) else {
            return;
        };
        let _lease = acquire_generation_read_lease(root, &generation_id)
            .unwrap()
            .release_publication_fence();
        fs::write(marker, b"ready").unwrap();
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn stale_read_marker_is_reconciled_and_malformed_marker_fails_closed() {
        let root = tempdir().unwrap();
        let slot = create_slot(root.path(), 'd');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(slot.clone(), None).unwrap(),
        )
        .unwrap();
        let lease = acquire_generation_read_lease(root.path(), slot.generation_id())
            .unwrap()
            .release_publication_fence();
        let stale = generation_read_lease_path(root.path(), &slot);
        assert!(stale.is_file());
        drop(lease);
        assert!(live_generation_read_lease_targets(root.path())
            .unwrap()
            .is_empty());
        assert!(!stale.exists());

        let malformed = root
            .path()
            .join(format!("{GENERATION_READ_LEASE_PREFIX}malformed.lock"));
        let file = File::create(&malformed).unwrap();
        restrict_private_file_handle(&file).unwrap();
        drop(file);
        assert!(matches!(
            live_generation_read_lease_targets(root.path()),
            Err(IndexError::InvalidGenerationRetentionLease)
        ));
        assert!(malformed.is_file(), "malformed state was silently removed");
    }

    #[test]
    fn reader_bypasses_candidate_build_but_waits_for_the_ownership_handoff() {
        let root = tempdir().unwrap();
        let slot = create_slot(root.path(), 'e');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(slot.clone(), None).unwrap(),
        )
        .unwrap();
        let directory = DurableMmapDirectory::open(root.path()).unwrap();
        let lock = Lock {
            filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
            is_blocking: false,
        };
        let writer = acquire_generation_writer_lock_with_retry(&directory, &lock).unwrap();
        let first_reader_root = root.path().to_path_buf();
        let (result_tx, result_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let result =
                acquire_active_generation_read_lease(first_reader_root).map(|acquisition| {
                    let generation_id = acquisition.target().generation_id().to_owned();
                    drop(acquisition.release_publication_fence());
                    generation_id
                });
            result_tx.send(result).unwrap();
        });

        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_millis(250))
                .unwrap()
                .unwrap(),
            slot.generation_id()
        );
        reader.join().unwrap();
        drop(writer);

        let ownership_fence = crate::acquire_generation_ownership_fence(root.path()).unwrap();
        let second_reader_root = root.path().to_path_buf();
        let (result_tx, result_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let result =
                acquire_active_generation_read_lease(second_reader_root).map(|acquisition| {
                    let generation_id = acquisition.target().generation_id().to_owned();
                    drop(acquisition.release_publication_fence());
                    generation_id
                });
            result_tx.send(result).unwrap();
        });
        assert!(
            result_rx.recv_timeout(Duration::from_millis(550)).is_err(),
            "reader crossed an active ownership handoff"
        );
        drop(ownership_fence);
        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            slot.generation_id()
        );
        reader.join().unwrap();
    }

    #[test]
    fn cross_process_lease_survives_gc_then_crash_is_reconciled() {
        let root = tempdir().unwrap();
        let old = create_slot(root.path(), 'a');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(old.clone(), None).unwrap(),
        )
        .unwrap();

        let marker = root.path().join("child-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("retention::tests::generation_read_lease_crash_child")
            .arg("--nocapture")
            .env(CHILD_ROOT, root.path())
            .env(CHILD_GENERATION, old.generation_id())
            .env(CHILD_MARKER, &marker)
            .spawn()
            .unwrap();
        for _ in 0..250 {
            if marker.is_file() {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "lease child exited early"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.is_file(), "lease child did not acquire its lock");

        let previous = create_slot(root.path(), 'b');
        let active = create_slot(root.path(), 'c');
        let pointer = ActiveGenerationPointer::new(active.clone(), Some(previous.clone())).unwrap();
        publish_active_generation_pointer(root.path(), &pointer).unwrap();
        let retained = vec![
            active.generation_id().to_owned(),
            previous.generation_id().to_owned(),
        ];
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(slot_path(root.path(), &old).is_dir());
        assert!(manifest_path(root.path(), old.generation_id()).is_file());
        assert!(certification_path(root.path(), &old).is_file());

        child.kill().unwrap();
        child.wait().unwrap();
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(!slot_path(root.path(), &old).exists());
        assert!(!manifest_path(root.path(), old.generation_id()).exists());
        assert!(!certification_path(root.path(), &old).exists());
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(old.generation_id())
        }));
    }
}
