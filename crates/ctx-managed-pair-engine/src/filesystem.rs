use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read as _, Write as _},
    ops::Deref,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    ManagedPairComponentIdentity, MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
    MANAGED_PAIR_STATE_RELATIVE_PATH,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    Core,
    Companion,
    Envelope,
    State,
}

impl Slot {
    pub(super) const ALL: [Self; 4] = [Self::Core, Self::Companion, Self::Envelope, Self::State];
    pub(super) const DATA: [Self; 3] = [Self::Core, Self::Companion, Self::Envelope];
    pub(super) const BACKUP_ORDER: [Self; 4] =
        [Self::State, Self::Core, Self::Companion, Self::Envelope];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Core => 0,
            Self::Companion => 1,
            Self::Envelope => 2,
            Self::State => 3,
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Core => "managed-pair Core component",
            Self::Companion => "managed-pair companion component",
            Self::Envelope => "managed-pair signed envelope",
            Self::State => "managed-pair state marker",
        }
    }

    pub(super) const fn backup_fault(self) -> &'static str {
        match self {
            Self::Core => "backup_core",
            Self::Companion => "backup_companion",
            Self::Envelope => "backup_envelope",
            Self::State => "backup_state",
        }
    }

    pub(super) const fn publish_fault(self) -> &'static str {
        match self {
            Self::Core => "publish_core",
            Self::Companion => "publish_companion",
            Self::Envelope => "publish_envelope",
            Self::State => "publish_state",
        }
    }

    fn relative_path(self) -> &'static str {
        match self {
            Self::Core if cfg!(windows) => "bin/ctx.exe",
            Self::Core => "bin/ctx",
            Self::Companion if cfg!(windows) => "libexec/ctx-pro.exe",
            Self::Companion => "libexec/ctx-pro",
            Self::Envelope => MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
            Self::State => MANAGED_PAIR_STATE_RELATIVE_PATH,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Layout {
    root: PathBuf,
    root_directory: Arc<SecureDirectory>,
    bin_directory: Arc<SecureDirectory>,
    libexec_directory: Arc<SecureDirectory>,
    share_directory: Arc<SecureDirectory>,
    ctx_directory: Arc<SecureDirectory>,
}

#[derive(Debug, Clone)]
pub(super) struct Entry {
    path: PathBuf,
    name: OsString,
    directory: Arc<SecureDirectory>,
}

impl Entry {
    fn new(path: PathBuf, directory: Arc<SecureDirectory>) -> Result<Self> {
        let name = file_name(&path, "managed-pair entry")?.to_os_string();
        Ok(Self {
            path,
            name,
            directory,
        })
    }

    fn sibling(&self, name: OsString) -> Self {
        Self {
            path: self.path.with_file_name(&name),
            name,
            directory: Arc::clone(&self.directory),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Entry {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Deref for Entry {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Layout {
    pub(super) fn open(root: &Path, create: bool) -> Result<Self> {
        validate_absolute_root(root, "managed-pair install root")?;
        if create {
            ensure_directory(root)?;
            ensure_directory(&root.join("bin"))?;
            ensure_directory(&root.join("libexec"))?;
            ensure_directory(&root.join("share"))?;
            ensure_directory(&root.join("share/ctx"))?;
        } else {
            validate_directory(root)?;
            validate_directory(&root.join("bin"))?;
            validate_directory(&root.join("libexec"))?;
            validate_directory(&root.join("share"))?;
            validate_directory(&root.join("share/ctx"))?;
        }
        Self::bind(root)
    }

    pub(super) fn open_candidate(root: &Path) -> Result<Self> {
        validate_absolute_root(root, "managed-pair candidate root")?;
        validate_directory(root)?;
        validate_directory(&root.join("bin"))?;
        validate_directory(&root.join("libexec"))?;
        validate_directory(&root.join("share"))?;
        validate_directory(&root.join("share/ctx"))?;
        Self::bind(root)
    }

    fn bind(root: &Path) -> Result<Self> {
        let root_directory = SecureDirectory::open(root)?;
        Self::bind_from_root(root, root_directory)
    }

    fn bind_from_root(root: &Path, root_directory: SecureDirectory) -> Result<Self> {
        let root_directory = Arc::new(root_directory);
        let bin_directory = Arc::new(root_directory.open_child_directory(OsStr::new("bin"))?);
        let libexec_directory =
            Arc::new(root_directory.open_child_directory(OsStr::new("libexec"))?);
        let share_directory = Arc::new(root_directory.open_child_directory(OsStr::new("share"))?);
        let ctx_directory = Arc::new(share_directory.open_child_directory(OsStr::new("ctx"))?);
        let layout = Self {
            root: root.to_path_buf(),
            root_directory,
            bin_directory,
            libexec_directory,
            share_directory,
            ctx_directory,
        };
        layout.revalidate()?;
        Ok(layout)
    }

    fn open_candidate_attempt(&self, attempt_id: &str) -> Result<(Self, SecureDirectory)> {
        if !super::journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair attempt ID is invalid");
        }
        let base = self
            .ctx_directory
            .open_child_directory(OsStr::new(".managed-pair-candidates"))?;
        let root_directory = base.open_child_directory(OsStr::new(attempt_id))?;
        let root = candidate_root(&self.root, attempt_id)?;
        Ok((Self::bind_from_root(&root, root_directory)?, base))
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        for (directory, path) in [
            (&self.root_directory, self.root.clone()),
            (&self.bin_directory, self.root.join("bin")),
            (&self.libexec_directory, self.root.join("libexec")),
            (&self.share_directory, self.root.join("share")),
            (&self.ctx_directory, self.root.join("share/ctx")),
        ] {
            directory.require_path_identity(&path)?;
        }
        Ok(())
    }

    pub(super) fn target(&self, slot: Slot) -> Entry {
        let directory = match slot {
            Slot::Core => Arc::clone(&self.bin_directory),
            Slot::Companion => Arc::clone(&self.libexec_directory),
            Slot::Envelope | Slot::State => Arc::clone(&self.ctx_directory),
        };
        Entry::new(self.root.join(slot.relative_path()), directory)
            .expect("fixed managed-pair slot has a file name")
    }

    pub(super) fn staged(&self, slot: Slot, attempt_id: &str) -> Entry {
        transaction_sibling(&self.target(slot), attempt_id, "new")
    }

    pub(super) fn backup(&self, slot: Slot, attempt_id: &str) -> Entry {
        transaction_sibling(&self.target(slot), attempt_id, "old")
    }

    pub(super) fn journal(&self) -> Entry {
        Entry::new(
            self.root
                .join("share/ctx/.managed-pair-transaction-v1.json"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair journal has a file name")
    }

    pub(super) fn journal_temporary(&self) -> Entry {
        Entry::new(
            self.root.join("share/ctx/.managed-pair-transaction-v1.tmp"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair temporary journal has a file name")
    }

    pub(super) fn lock(&self) -> Entry {
        Entry::new(
            self.root.join(".managed-pair-transaction-v1.lock"),
            Arc::clone(&self.root_directory),
        )
        .expect("fixed managed-pair lock has a file name")
    }

    pub(super) fn uninstall_journal(&self) -> Entry {
        Entry::new(
            self.root.join("share/ctx/.managed-pair-uninstall-v1.json"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair uninstall journal has a file name")
    }

    pub(super) fn uninstall_journal_temporary(&self) -> Entry {
        Entry::new(
            self.root.join("share/ctx/.managed-pair-uninstall-v1.tmp"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair uninstall temporary has a file name")
    }

    pub(super) fn begin_record(&self) -> Entry {
        Entry::new(
            self.root.join("share/ctx/.managed-pair-begin-v1.json"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair begin record has a file name")
    }

    pub(super) fn terminal_receipt(&self) -> Entry {
        Entry::new(
            self.root
                .join("share/ctx/.managed-pair-last-attempt-v1.json"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair receipt has a file name")
    }

    pub(super) fn terminal_receipt_temporary(&self) -> Entry {
        Entry::new(
            self.root
                .join("share/ctx/.managed-pair-last-attempt-v1.tmp"),
            Arc::clone(&self.ctx_directory),
        )
        .expect("fixed managed-pair receipt temporary has a file name")
    }

    pub(super) fn remove_empty_candidate_base(&self) -> Result<()> {
        match self
            .ctx_directory
            .remove_directory(OsStr::new(".managed-pair-candidates"))
        {
            Ok(()) => self.ctx_directory.sync(),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error).context("remove empty managed-pair candidate base"),
        }
    }
}

pub(super) fn candidate_root(install_root: &Path, attempt_id: &str) -> Result<PathBuf> {
    if !super::journal::valid_attempt_id(attempt_id) {
        bail!("managed-pair attempt ID is invalid");
    }
    Ok(install_root
        .join("share/ctx/.managed-pair-candidates")
        .join(attempt_id))
}

pub(super) fn create_candidate(install_root: &Path, attempt_id: &str) -> Result<PathBuf> {
    let base = install_root.join("share/ctx/.managed-pair-candidates");
    ensure_directory(&base)?;
    let root = candidate_root(install_root, attempt_id)?;
    ensure_directory(&root)?;
    Layout::open(&root, true)?;
    Ok(root)
}

pub(super) fn candidate_exists(layout: &Layout, attempt_id: &str) -> Result<bool> {
    match layout.open_candidate_attempt(attempt_id) {
        Ok(_) => Ok(true),
        Err(error) if is_not_found(&error) => Ok(false),
        Err(error) => Err(error).context("inspect managed-pair candidate root"),
    }
}

pub(super) fn remove_candidate(layout: &Layout, attempt_id: &str) -> Result<()> {
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

pub(super) fn legacy_journal_present(install_root: &Path) -> Result<bool> {
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

fn transaction_sibling(target: &Entry, attempt_id: &str, suffix: &str) -> Entry {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-pair");
    target.sibling(format!(".{name}.managed-pair-{attempt_id}.{suffix}").into())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct FileStamp {
    pub(super) device: u64,
    pub(super) file: u64,
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
}

pub(super) struct ObservedFile {
    pub(super) bytes: Vec<u8>,
    pub(super) stamp: FileStamp,
}

pub(super) fn validate_absolute_root(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("{label} must be a safe absolute path: {}", path.display());
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow!("managed-pair directory has no parent"))?;
            if !parent.as_os_str().is_empty() && !parent.exists() {
                ensure_directory(parent)?;
            }
            validate_directory(parent)?;
            fs::create_dir(path)
                .with_context(|| format!("create managed-pair directory {}", path.display()))?;
            protect_directory(path)?;
            validate_directory(path)
        }
        Err(error) => Err(error).with_context(|| format!("inspect directory {}", path.display())),
    }
}

fn validate_directory(path: &Path) -> Result<()> {
    let directory = SecureDirectory::open(path)?;
    let metadata = directory.file.metadata()?;
    if !metadata.is_dir() {
        bail!("managed-pair path is not a directory: {}", path.display());
    }
    Ok(())
}

fn protect_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    ctx_history_platform::platform_security::restrict_private_directory(path)?;
    Ok(())
}

pub(super) struct TransactionLock {
    _file: File,
    #[cfg(windows)]
    _root_guard: File,
}

pub(super) fn acquire_lock(layout: &Layout) -> Result<TransactionLock> {
    use fs2::FileExt as _;

    layout.revalidate()?;
    let entry = layout.lock();
    let file = match open_owner_lock(&entry, "managed-pair transaction lock") {
        Ok(file) => file,
        Err(error) if is_not_found(&error) => {
            let file = entry.directory.create_lock(&entry.name, entry.path())?;
            protect_file_handle(&file, false)?;
            file.sync_all()?;
            entry.directory.sync()?;
            file
        }
        Err(error) => return Err(error),
    };
    file.lock_exclusive()
        .context("lock managed-pair installation transaction")?;
    require_open_named_identity(&entry, &file, "managed-pair transaction lock")?;
    #[cfg(windows)]
    let root_guard = layout
        .root_directory
        .guard_path_identity(&layout.root)
        .context("retain stable managed-pair installation authority")?;
    layout.revalidate()?;
    Ok(TransactionLock {
        _file: file,
        #[cfg(windows)]
        _root_guard: root_guard,
    })
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

pub(super) fn read_regular(entry: &Entry, max: u64, label: &str) -> Result<ObservedFile> {
    observe_regular(entry, max, label, true, false)
}

pub(super) fn read_temporary(entry: &Entry, max: u64, label: &str) -> Result<Option<ObservedFile>> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_none()
    {
        return Ok(None);
    }
    observe_regular(entry, max, label, true, true).map(Some)
}

fn observe_regular(
    entry: &Entry,
    max: u64,
    label: &str,
    collect: bool,
    allow_empty: bool,
) -> Result<ObservedFile> {
    let mut file = open_owner_regular(entry, label)?;
    let (device, identity, size_bytes) = file_information(&file, label)?;
    if (!allow_empty && size_bytes == 0) || size_bytes > max {
        bail!("{label} size is outside its bound");
    }
    let mut bytes = if collect {
        let capacity = usize::try_from(size_bytes).context("managed-pair file is too large")?;
        Vec::with_capacity(capacity)
    } else {
        Vec::new()
    };
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count)?)
            .ok_or_else(|| anyhow!("{label} size overflow"))?;
        if total > size_bytes || total > max {
            bail!("{label} changed size while being read");
        }
        hasher.update(&buffer[..count]);
        if collect {
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    if total != size_bytes {
        bail!("{label} changed size while being read");
    }
    let stamp = FileStamp {
        device,
        file: identity,
        size_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    };
    require_named_identity(entry, &stamp, label)?;
    Ok(ObservedFile { bytes, stamp })
}

pub(super) fn copy_verified(
    source: &Entry,
    target: &Entry,
    expected: &ManagedPairComponentIdentity,
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    let mut source_file = open_owner_regular(source, label)?;
    let (source_device, source_identity, source_size) = file_information(&source_file, label)?;
    if source_size != expected.size_bytes() {
        bail!("{label} size does not match the verified managed-pair identity");
    }
    let mut target_file = create_new_file(target, executable, label)?;
    let copy_result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = source_file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(count)?)
                .ok_or_else(|| anyhow!("{label} size overflow"))?;
            if total > expected.size_bytes() {
                bail!("{label} grew while being copied");
            }
            hasher.update(&buffer[..count]);
            target_file.write_all(&buffer[..count])?;
        }
        if total != expected.size_bytes() || format!("{:x}", hasher.finalize()) != expected.sha256()
        {
            bail!("{label} does not match the verified managed-pair identity");
        }
        target_file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = copy_result {
        drop(target_file);
        remove_untrusted_new_file(target);
        return Err(error);
    }
    drop(target_file);
    sync_parent(target)?;
    let source_stamp = FileStamp {
        device: source_device,
        file: source_identity,
        size_bytes: source_size,
        sha256: expected.sha256().to_owned(),
    };
    require_named_identity(source, &source_stamp, label)?;
    let target_stamp = observe_regular(target, expected.size_bytes(), label, false, false)?.stamp;
    if target_stamp.size_bytes != expected.size_bytes() || target_stamp.sha256 != expected.sha256()
    {
        bail!("staged {label} changed after its verified copy");
    }
    Ok(target_stamp)
}

pub(super) fn copy_exact(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    require_stamp(source, expected, max, label)?;
    let mut source_file = open_owner_regular(source, label)?;
    require_file_identity(&source_file, expected, label)?;
    let mut target_file = create_new_file(target, executable, label)?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = source_file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(count)?)
                .ok_or_else(|| anyhow!("{label} backup size overflow"))?;
            if total > expected.size_bytes || total > max {
                bail!("{label} changed while its rollback backup was copied");
            }
            hasher.update(&buffer[..count]);
            target_file.write_all(&buffer[..count])?;
        }
        if total != expected.size_bytes || format!("{:x}", hasher.finalize()) != expected.sha256 {
            bail!("{label} changed while its rollback backup was copied");
        }
        target_file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(target_file);
        remove_untrusted_new_file(target);
        return Err(error);
    }
    drop(target_file);
    sync_parent(target)?;
    require_stamp(source, expected, max, label)?;
    let backup = observe_regular(target, max, label, false, false)?.stamp;
    if backup.size_bytes != expected.size_bytes || backup.sha256 != expected.sha256 {
        bail!("{label} rollback backup does not match the original bytes");
    }
    Ok(backup)
}

pub(super) fn write_new(
    entry: &Entry,
    bytes: &[u8],
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    if bytes.is_empty() {
        bail!("{label} must not be empty");
    }
    let mut file = create_new_file(entry, executable, label)?;
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = write_result {
        drop(file);
        remove_untrusted_new_file(entry);
        return Err(error).with_context(|| format!("write staged {label}"));
    }
    drop(file);
    sync_parent(entry)?;
    let observed = observe_regular(
        entry,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        label,
        true,
        false,
    )?;
    let expected_sha = format!("{:x}", Sha256::digest(bytes));
    if observed.bytes != bytes || observed.stamp.sha256 != expected_sha {
        bail!("staged {label} changed while being written");
    }
    Ok(observed.stamp)
}

fn create_new_file(entry: &Entry, executable: bool, label: &str) -> Result<File> {
    let file = entry
        .directory
        .create_new(&entry.name, entry.path(), executable)
        .with_context(|| format!("create staged {label} {}", entry.display()))?;
    if let Err(error) = protect_file_handle(&file, executable) {
        drop(file);
        remove_untrusted_new_file(entry);
        return Err(error).with_context(|| format!("protect staged {label}"));
    }
    Ok(file)
}

fn remove_untrusted_new_file(entry: &Entry) {
    let _ = entry.directory.remove_file(&entry.name, entry.path());
}

fn protect_file_handle(file: &File, executable: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(if executable {
            0o700
        } else {
            0o600
        }))?;
    }
    #[cfg(windows)]
    {
        let _ = executable;
        ctx_history_platform::platform_security::restrict_private_file_handle(file)?;
    }
    Ok(())
}

pub(super) fn verify_content(
    entry: &Entry,
    expected: &ManagedPairComponentIdentity,
    label: &str,
) -> Result<()> {
    let observed = observe_regular(entry, expected.size_bytes(), label, false, false)?;
    if observed.stamp.size_bytes != expected.size_bytes()
        || observed.stamp.sha256 != expected.sha256()
    {
        bail!("{label} does not match its verified managed-pair identity");
    }
    Ok(())
}

pub(super) fn stamp_optional(entry: &Entry, max: u64, label: &str) -> Result<Option<FileStamp>> {
    stamp_optional_impl(entry, max, label, false)
}

fn stamp_optional_impl(
    entry: &Entry,
    max: u64,
    label: &str,
    allow_empty: bool,
) -> Result<Option<FileStamp>> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(
        observe_regular(entry, max, label, false, allow_empty)?.stamp,
    ))
}

pub(super) fn matches_stamp(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<bool> {
    Ok(stamp_optional(entry, max, label)?.as_ref() == Some(expected))
}

pub(super) fn require_stamp(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    if !matches_stamp(entry, expected, max, label)? {
        bail!("{label} was substituted at {}", entry.display());
    }
    Ok(())
}

pub(super) fn require_absent(entry: &Entry, label: &str) -> Result<()> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_some()
    {
        bail!("unexpected {label} exists at {}", entry.display());
    }
    Ok(())
}

pub(super) fn rename_exact(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    if target
        .directory
        .entry_metadata(&target.name, target.path())?
        .is_some()
    {
        bail!("unexpected {label} exists at {}", target.display());
    }
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, false).with_context(|| {
        format!(
            "rename {label} {} to {}",
            source.display(),
            target.display()
        )
    })?;
    require_stamp(target, expected, max, label)?;
    target.directory.sync()
}

pub(super) fn remove_if_exact(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    remove_if_exact_impl(entry, expected, max, label, false)
}

pub(super) fn remove_temporary_exact(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    remove_if_exact_impl(entry, expected, max, label, true)
}

fn remove_if_exact_impl(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
    allow_empty: bool,
) -> Result<()> {
    let Some(actual) = stamp_optional_impl(entry, max, label, allow_empty)? else {
        return Ok(());
    };
    if &actual != expected {
        bail!(
            "refusing to remove substituted {label} at {}",
            entry.display()
        );
    }
    require_named_identity(entry, expected, label)?;
    remove_entry_exact(entry, expected, label)
        .with_context(|| format!("remove {label} {}", entry.display()))?;
    entry.directory.sync()
}

#[cfg(unix)]
fn remove_entry_exact(entry: &Entry, _expected: &FileStamp, _label: &str) -> Result<()> {
    entry.directory.remove_file(&entry.name, entry.path())
}

#[cfg(windows)]
fn remove_entry_exact(entry: &Entry, expected: &FileStamp, label: &str) -> Result<()> {
    use std::{mem::size_of, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let file = open_owner_regular_for_delete(entry, label)?;
    require_file_identity(&file, expected, label)?;
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO>())?,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("unlink managed-pair file by handle");
    }
    Ok(())
}

fn open_owner_regular(entry: &Entry, label: &str) -> Result<File> {
    let file = entry
        .directory
        .open_file(&entry.name, entry.path())
        .with_context(|| format!("open {label} {}", entry.display()))?;
    validate_open_owner_regular(entry, &file, label)?;
    Ok(file)
}

fn open_owner_lock(entry: &Entry, label: &str) -> Result<File> {
    let file = entry
        .directory
        .open_lock(&entry.name, entry.path())
        .with_context(|| format!("open {label} {}", entry.display()))?;
    validate_open_owner_regular(entry, &file, label)?;
    Ok(file)
}

#[cfg(windows)]
fn open_owner_regular_for_delete(entry: &Entry, label: &str) -> Result<File> {
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, READ_CONTROL, SYNCHRONIZE,
    };

    // First validate and pin the exact no-follow, owner-private object while
    // deletion is denied. The mutable reopen is relative to the same retained
    // directory and must resolve to that exact identity before it is used.
    let pinned = open_owner_regular(entry, label)?;
    let expected = file_information(&pinned, label)?;
    drop(pinned);
    let file = entry
        .directory
        .open_relative(
            &entry.name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | DELETE | SYNCHRONIZE,
            FILE_SHARE_READ,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
        )
        .with_context(|| format!("open mutable {label} {}", entry.display()))?;
    validate_open_owner_regular_handle(&file, label)?;
    if file_information(&file, label)? != expected {
        bail!("{label} was substituted before its mutable open");
    }
    Ok(file)
}

fn validate_open_owner_regular(entry: &Entry, file: &File, label: &str) -> Result<()> {
    let metadata = file.metadata()?;
    let named = entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .ok_or_else(|| anyhow!("{label} disappeared while being opened"))?;
    if !metadata.is_file() || !named.is_file || named.is_symlink {
        bail!(
            "{label} is not a regular no-follow file: {}",
            entry.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.dev() != named.device
            || metadata.ino() != named.file
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            bail!(
                "{label} is not an owner-safe unique file: {}",
                entry.display()
            );
        }
    }
    #[cfg(windows)]
    {
        if named.attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            bail!(
                "{label} traverses a Windows reparse point: {}",
                entry.display()
            );
        }
        let (device, identity, links) = windows_file_information(&file, label)?;
        if device != named.device || identity != named.file || links != 1 {
            bail!(
                "{label} is not an owner-safe unique Windows file: {}",
                entry.display()
            );
        }
        validate_open_owner_regular_handle(file, label)?;
    }
    Ok(())
}

#[cfg(windows)]
fn validate_open_owner_regular_handle(file: &File, label: &str) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = file.metadata()?;
    let (_, _, links) = windows_file_information(file, label)?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || links != 1
    {
        bail!("{label} is not an owner-safe unique no-follow Windows file");
    }
    ctx_history_platform::platform_security::verify_private_file_handle(file)
        .with_context(|| format!("verify owner-safe {label}"))
}

fn require_named_identity(entry: &Entry, expected: &FileStamp, label: &str) -> Result<()> {
    let file = open_owner_regular(entry, label)?;
    require_file_identity(&file, expected, label)
}

fn require_file_identity(file: &File, expected: &FileStamp, label: &str) -> Result<()> {
    let (device, identity, size_bytes) = file_information(file, label)?;
    if device != expected.device || identity != expected.file || size_bytes != expected.size_bytes {
        bail!("{label} pathname changed while being verified");
    }
    Ok(())
}

fn require_open_named_identity(entry: &Entry, file: &File, label: &str) -> Result<()> {
    let (device, identity, _) = file_information(file, label)?;
    let named = entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .ok_or_else(|| anyhow!("{label} disappeared while being locked"))?;
    if !named.is_file || named.is_symlink || named.device != device || named.file != identity {
        bail!("{label} pathname changed while being locked");
    }
    Ok(())
}

fn file_name<'a>(path: &'a Path, label: &str) -> Result<&'a OsStr> {
    path.file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("{label} path has no file name"))
}

mod secure_directory;

use secure_directory::SecureDirectory;

#[cfg(unix)]
fn file_information(file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino(), metadata.len()))
}

#[cfg(windows)]
fn file_information(file: &File, label: &str) -> Result<(u64, u64, u64)> {
    let (device, identity, _) = windows_file_information(file, label)?;
    Ok((device, identity, file.metadata()?.len()))
}

#[cfg(windows)]
fn windows_file_information(file: &File, label: &str) -> Result<(u64, u64, u32)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("inspect {label}"));
    }
    let information = unsafe { information.assume_init() };
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        information.nNumberOfLinks,
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_information(_file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    bail!("managed-pair file identity is unsupported on this platform")
}

#[cfg(unix)]
fn durable_rename(
    source_entry: &Entry,
    target_entry: &Entry,
    _expected: &FileStamp,
    _label: &str,
    _replace: bool,
) -> Result<()> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
    };
    let source = CString::new(source_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair source name contains a NUL"))?;
    let target = CString::new(target_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair target name contains a NUL"))?;
    if unsafe {
        libc::renameat(
            source_entry.directory.file.as_raw_fd(),
            source.as_ptr(),
            target_entry.directory.file.as_raw_fd(),
            target.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("rename managed-pair file");
    }
    Ok(())
}

#[cfg(windows)]
fn durable_rename(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    label: &str,
    replace: bool,
) -> Result<()> {
    use std::{
        mem::size_of,
        os::windows::{ffi::OsStrExt as _, io::AsRawHandle as _},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    let file = open_owner_regular_for_delete(source, label)?;
    require_file_identity(&file, expected, label)?;
    let name: Vec<u16> = target.name.encode_wide().collect();
    if name.is_empty() || name.contains(&0) {
        bail!("managed-pair target name is invalid");
    }
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| anyhow!("managed-pair target name is too long"))?;
    // Windows documents FileNameLength without the terminator, while the
    // FILE_RENAME_INFO buffer itself must include its trailing WCHAR storage.
    // The zero-filled tail therefore supplies the required terminator.
    let total_bytes = size_of::<FILE_RENAME_INFO>()
        .checked_add(name_bytes)
        .ok_or_else(|| anyhow!("managed-pair rename buffer is too large"))?;
    let words = total_bytes.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = replace;
        (*information).RootDirectory = target.directory.file.as_raw_handle().cast();
        (*information).FileNameLength = u32::try_from(name_bytes)?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            name.len(),
        );
    }
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileRenameInfo,
            information.cast(),
            u32::try_from(total_bytes)?,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("rename managed-pair file by handle");
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    require_stamp(target, expected, max, label)
}

#[cfg(unix)]
pub(super) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    target.directory.sync()?;
    require_stamp(target, expected, max, label)
}

pub(super) fn sync_parent(entry: &Entry) -> Result<()> {
    entry.directory.sync()
}

#[cfg(windows)]
pub(super) fn current_process_creation_identity() -> Result<u64> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    process_creation_identity(unsafe { GetCurrentProcess() })
}

#[cfg(windows)]
pub(super) fn wait_for_parent_exit(parent_pid: u32, parent_creation_time: u64) -> Result<()> {
    wait_for_parent_exit_with_timeout(parent_pid, parent_creation_time, 5 * 60 * 1_000)
}

#[cfg(windows)]
fn wait_for_parent_exit_with_timeout(
    parent_pid: u32,
    parent_creation_time: u64,
    timeout_ms: u32,
) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };
    if parent_pid == 0 || parent_pid == std::process::id() || parent_creation_time == 0 {
        bail!("managed-pair swapper has an invalid parent identity");
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            parent_pid,
        )
    };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(error).context("open managed-pair parent process");
    }
    let observed_creation_time = process_creation_identity(handle);
    if observed_creation_time
        .as_ref()
        .is_ok_and(|observed| *observed != parent_creation_time)
    {
        unsafe { CloseHandle(handle) };
        return Ok(());
    }
    observed_creation_time?;
    let status = unsafe { WaitForSingleObject(handle, timeout_ms) };
    unsafe { CloseHandle(handle) };
    match status {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => bail!("timed out waiting for the managed-pair parent process to exit"),
        WAIT_FAILED => {
            Err(std::io::Error::last_os_error()).context("wait for managed-pair parent process")
        }
        other => bail!("unexpected managed-pair parent wait status {other}"),
    }
}

#[cfg(windows)]
fn process_creation_identity(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<u64> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};

    let mut creation = MaybeUninit::<FILETIME>::zeroed();
    let mut exit = MaybeUninit::<FILETIME>::zeroed();
    let mut kernel = MaybeUninit::<FILETIME>::zeroed();
    let mut user = MaybeUninit::<FILETIME>::zeroed();
    if unsafe {
        GetProcessTimes(
            handle,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read managed-pair parent creation identity");
    }
    let creation = unsafe { creation.assume_init() };
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(all(test, windows))]
pub(super) fn process_creation_identity_for_test(parent_pid: u32) -> Result<u64> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent_pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("open test parent process");
    }
    let identity = process_creation_identity(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    identity
}

#[cfg(all(test, windows))]
pub(super) fn wait_for_parent_exit_for_test(
    parent_pid: u32,
    parent_creation_time: u64,
    timeout_ms: u32,
) -> Result<()> {
    wait_for_parent_exit_with_timeout(parent_pid, parent_creation_time, timeout_ms)
}
