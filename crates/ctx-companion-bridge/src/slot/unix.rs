use std::{
    ffi::{CString, OsStr},
    fs::{self, File},
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
        },
    },
    path::{Component, Path, PathBuf},
};

#[cfg(not(target_os = "linux"))]
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::{
    identity::{FileIdentity, PairIdentity, Sha256Digest},
    BridgeError,
};

use super::{PreparedPair, SlotPaths, MAX_COMPONENT_BYTES};

#[cfg(not(target_os = "linux"))]
static SNAPSHOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct ExecutionBinding {
    root: File,
    executable: File,
    trusted_owner: u32,
    #[cfg(target_os = "linux")]
    sealed: bool,
}

impl ExecutionBinding {
    pub(crate) fn program(&self) -> PathBuf {
        let descriptor_root = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        Path::new(descriptor_root).join(self.executable.as_raw_fd().to_string())
    }

    pub(crate) fn execution_fd(&self) -> RawFd {
        self.executable.as_raw_fd()
    }

    pub(crate) fn root_fd(&self) -> RawFd {
        self.root.as_raw_fd()
    }

    pub(crate) fn verify_retained(&self) -> Result<(), BridgeError> {
        #[cfg(target_os = "linux")]
        {
            if !self.sealed {
                return Err(BridgeError::InvalidSlot(
                    "retained companion snapshot is not sealed",
                ));
            }
            let seals = unsafe { libc::fcntl(self.executable.as_raw_fd(), libc::F_GET_SEALS) };
            let required =
                libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
            if seals == -1 || seals & required != required {
                return Err(BridgeError::filesystem(
                    "verify retained companion seals",
                    io::Error::last_os_error(),
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn read_owner_safe_file(
        &self,
        relative: &[&str],
        maximum: usize,
    ) -> Result<Vec<u8>, BridgeError> {
        if relative.len() < 2 {
            return Err(BridgeError::InvalidSlot("shared path is incomplete"));
        }
        let mut directory = self
            .root
            .try_clone()
            .map_err(|error| BridgeError::filesystem("clone managed-root handle", error))?;
        for component in &relative[..relative.len() - 1] {
            let next = open_directory_at(directory.as_raw_fd(), OsStr::new(component)).map_err(
                |error| BridgeError::filesystem("open owner-safe shared directory", error),
            )?;
            verify_directory(&next, self.trusted_owner).map_err(|error| {
                BridgeError::filesystem("verify owner-safe shared directory", error)
            })?;
            directory = next;
        }
        let file = open_file_at(
            directory.as_raw_fd(),
            OsStr::new(relative[relative.len() - 1]),
        )
        .map_err(|error| BridgeError::filesystem("open owner-safe shared file", error))?;
        verify_regular_file(&file, self.trusted_owner, false)
            .map_err(|error| BridgeError::filesystem("verify owner-safe shared file", error))?;
        read_bounded(&file, maximum)
            .map_err(|error| BridgeError::filesystem("read owner-safe shared file", error))
    }
}

pub(super) fn prepare(paths: SlotPaths) -> Result<PreparedPair, BridgeError> {
    let root = open_absolute_directory(&paths.root).map_err(|error| {
        BridgeError::filesystem("open managed root without following links", error)
    })?;
    let bin = open_directory_at(root.as_raw_fd(), OsStr::new("bin"))
        .map_err(|error| BridgeError::filesystem("open fixed bin directory", error))?;
    let libexec = open_directory_at(root.as_raw_fd(), OsStr::new("libexec"))
        .map_err(|error| BridgeError::filesystem("open fixed libexec directory", error))?;
    let launcher = open_file_at(bin.as_raw_fd(), OsStr::new(super::core_filename()))
        .map_err(|error| BridgeError::filesystem("open fixed Core executable", error))?;
    let companion = open_file_at(libexec.as_raw_fd(), OsStr::new(super::companion_filename()))
        .map_err(|error| BridgeError::filesystem("open fixed companion executable", error))?;

    let launcher_metadata = launcher
        .metadata()
        .map_err(|error| BridgeError::filesystem("inspect fixed Core executable", error))?;
    let trusted_owner = launcher_metadata.uid();
    let effective_user = unsafe { libc::geteuid() };
    if trusted_owner != effective_user && trusted_owner != 0 {
        return Err(BridgeError::InvalidSlot(
            "managed pair owner is neither the current user nor root",
        ));
    }
    for directory in [&root, &bin, &libexec] {
        verify_directory(directory, trusted_owner).map_err(|error| {
            BridgeError::filesystem("verify managed directory ownership", error)
        })?;
    }
    verify_regular_file(&launcher, trusted_owner, true)
        .map_err(|error| BridgeError::filesystem("verify fixed Core executable", error))?;
    verify_regular_file(&companion, trusted_owner, true)
        .map_err(|error| BridgeError::filesystem("verify fixed companion executable", error))?;

    let launcher_digest = digest_file(&launcher, MAX_COMPONENT_BYTES)
        .map_err(|error| BridgeError::filesystem("stream-hash fixed Core executable", error))?;
    let (snapshot, companion_digest) = snapshot_companion(&companion, MAX_COMPONENT_BYTES)
        .map_err(|error| BridgeError::filesystem("snapshot fixed companion executable", error))?;
    let launcher_identity = unix_identity(&launcher, launcher_digest)
        .map_err(|error| BridgeError::filesystem("bind fixed Core file identity", error))?;
    let companion_identity = unix_identity(&companion, companion_digest)
        .map_err(|error| BridgeError::filesystem("bind fixed companion file identity", error))?;
    let identity = PairIdentity::new(paths.root.clone(), launcher_identity, companion_identity);
    Ok(PreparedPair {
        identity,
        execution: ExecutionBinding {
            root,
            executable: snapshot,
            trusted_owner,
            #[cfg(target_os = "linux")]
            sealed: true,
        },
    })
}

fn open_absolute_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not absolute",
        ));
    }
    let mut current = open_directory_path(Path::new("/"))?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => current = open_directory_at(current.as_raw_fd(), name)?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "managed root contains an unsupported component",
                ));
            }
        }
    }
    Ok(current)
}

fn open_directory_path(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)
}

fn open_directory_at(parent: RawFd, name: &OsStr) -> io::Result<File> {
    open_at(
        parent,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
    )
}

fn open_file_at(parent: RawFd, name: &OsStr) -> io::Result<File> {
    open_at(
        parent,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
}

fn open_at(parent: RawFd, name: &OsStr, flags: libc::c_int) -> io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn verify_directory(file: &File, owner: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed directory is not owner-safe",
        ));
    }
    Ok(())
}

fn verify_regular_file(file: &File, owner: u32, executable: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.permissions().mode();
    let owner_can_execute = owner == unsafe { libc::geteuid() } && mode & 0o100 != 0;
    let root_file_is_publicly_executable = owner == 0 && mode & 0o001 != 0;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner
        || mode & 0o022 != 0
        || (executable && !owner_can_execute && !root_file_is_publicly_executable)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed file is not owner-safe regular executable data",
        ));
    }
    Ok(())
}

fn unix_identity(file: &File, digest: Sha256Digest) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    Ok(FileIdentity::unix(
        metadata.len(),
        digest,
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
    ))
}

fn digest_file(file: &File, maximum: u64) -> io::Result<Sha256Digest> {
    let metadata = file.metadata()?;
    if metadata.len() == 0 || metadata.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed component is outside executable size bounds",
        ));
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "component grew while hashing",
            ));
        }
        digest.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "component changed while hashing",
        ));
    }
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

fn snapshot_companion(source: &File, maximum: u64) -> io::Result<(File, Sha256Digest)> {
    let metadata = source.metadata()?;
    if metadata.len() == 0 || metadata.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "companion is outside executable size bounds",
        ));
    }
    let mut snapshot = create_snapshot_file()?;
    let mut reader = source.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "companion grew while hashing",
            ));
        }
        snapshot.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "companion changed while hashing",
        ));
    }
    snapshot.flush()?;
    snapshot.seek(SeekFrom::Start(0))?;
    snapshot.set_permissions(fs::Permissions::from_mode(0o500))?;
    seal_snapshot(&snapshot)?;
    Ok((snapshot, Sha256Digest::from_bytes(digest.finalize().into())))
}

#[cfg(target_os = "linux")]
fn create_snapshot_file() -> io::Result<File> {
    let name = c"ctx-companion-bridge";
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor as RawFd) })
    }
}

#[cfg(not(target_os = "linux"))]
fn create_snapshot_file() -> io::Result<File> {
    let sequence = SNAPSHOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut template = format!(
        "/tmp/ctx-companion-bridge-{}-{sequence}-XXXXXX\0",
        std::process::id()
    )
    .into_bytes();
    let descriptor = unsafe { libc::mkstemp(template.as_mut_ptr().cast()) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let unlink_result = unsafe { libc::unlink(template.as_ptr().cast()) };
    if unlink_result != 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        return Err(error);
    }
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == -1
        || unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
    {
        let error = io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        return Err(error);
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
fn seal_snapshot(file: &File) -> io::Result<()> {
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
fn seal_snapshot(_file: &File) -> io::Result<()> {
    Ok(())
}

fn read_bounded(file: &File, maximum: usize) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if metadata.len() == 0 || metadata.len() > maximum as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed file is outside size bounds",
        ));
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    reader.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum || bytes.len() as u64 != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed file changed while reading",
        ));
    }
    Ok(bytes)
}
