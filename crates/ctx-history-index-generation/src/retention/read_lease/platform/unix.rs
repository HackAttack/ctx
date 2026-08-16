use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::fs::{FileExt as _, MetadataExt as _},
    },
};

use ctx_history_platform::platform_security::{
    restrict_private_file_handle, verify_private_file_handle,
};

use super::LockKind;
use crate::read_root::OpenedDirectory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
pub(crate) struct OpenedCoordinator {
    root: File,
    file: File,
    root_identity: ObjectIdentity,
    file_identity: ObjectIdentity,
}

impl OpenedCoordinator {
    pub(crate) fn open(
        root_directory: &OpenedDirectory,
        name: &str,
        initialization_name: &str,
        magic: &[u8],
        create: bool,
    ) -> io::Result<Self> {
        let root = root_directory.try_clone_file()?;
        verify_private_root(&root)?;
        let root_identity = identity(&root)?;
        let name = path_component(name)?;
        let initialization_name = path_component(initialization_name)?;
        let initialization = open_initialization_file(&root, &initialization_name, create)?;
        initialization.sync_all()?;
        root.sync_all()?;
        lock_initialization(&initialization)?;
        let (file, created) = match open_at(&root, &name, false) {
            Ok(file) => (file, false),
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match open_at(&root, &name, true) {
                    Ok(file) => (file, true),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        (open_at(&root, &name, false)?, false)
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        let needs_initialization = if created {
            restrict_private_file_handle(&file)?;
            true
        } else {
            match verify_coordinator_file(&file, magic) {
                Ok(()) => false,
                Err(_) if create && recoverable_coordinator_prefix(&file, magic)? => true,
                Err(error) => return Err(error),
            }
        };
        if needs_initialization {
            #[cfg(test)]
            if let Some(prefix_len) =
                super::super::coordinator_creation_crash_prefix_len(magic.len())
            {
                write_exact_at(&file, &magic[..prefix_len])?;
                file.sync_all()?;
                root.sync_all()?;
                std::process::exit(super::super::COORDINATOR_CREATION_CRASH_EXIT_CODE);
            }
            write_exact_at(&file, magic)?;
            file.sync_all()?;
            root.sync_all()?;
        }
        verify_coordinator_file(&file, magic)?;
        let file_identity = identity(&file)?;
        let opened = Self {
            root,
            file,
            root_identity,
            file_identity,
        };
        opened.verify_binding(name.to_str().unwrap_or_default(), magic)?;
        Ok(opened)
    }

    pub(crate) fn verify_binding(&self, name: &str, magic: &[u8]) -> io::Result<()> {
        verify_private_root(&self.root)?;
        if identity(&self.root)? != self.root_identity {
            return Err(invalid_state());
        }
        verify_coordinator_file(&self.file, magic)?;
        if identity(&self.file)? != self.file_identity {
            return Err(invalid_state());
        }
        let name = path_component(name)?;
        let named = identity_at(&self.root, &name)?;
        if named != self.file_identity {
            return Err(invalid_state());
        }
        Ok(())
    }

    pub(crate) fn try_lock(&self, offset: u64, kind: LockKind) -> io::Result<bool> {
        set_lock(&self.file, offset, kind)
    }

    pub(crate) fn unlock(&self, offset: u64) -> io::Result<()> {
        unlock(&self.file, offset)
    }
}

fn open_initialization_file(root: &File, name: &CString, create: bool) -> io::Result<File> {
    let (file, created) = match open_at(root, name, false) {
        Ok(file) => (file, false),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            match open_at(root, name, true) {
                Ok(file) => (file, true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    (open_at(root, name, false)?, false)
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if created {
        restrict_private_file_handle(&file)?;
    }
    verify_initialization_file(&file)?;
    Ok(file)
}

fn open_at(root: &File, name: &CString, create: bool) -> io::Result<File> {
    let mut flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if create {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }
    // SAFETY: `root` and the NUL-terminated component remain live; a successful
    // descriptor is transferred into `File` exactly once.
    let descriptor = unsafe { libc::openat(root.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn verify_private_root(root: &File) -> io::Result<()> {
    let metadata = root.metadata()?;
    if metadata.is_dir()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o022 == 0
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid generation lease root: mode={:#o} uid={} euid={}",
                metadata.mode(),
                metadata.uid(),
                unsafe { libc::geteuid() }
            ),
        ))
    }
}

fn verify_coordinator_file(file: &File, magic: &[u8]) -> io::Result<()> {
    verify_private_file_handle(file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.len() != magic.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid generation lease file: mode={:#o} uid={} links={} len={}",
                metadata.mode(),
                metadata.uid(),
                metadata.nlink(),
                metadata.len()
            ),
        ));
    }
    let mut bytes = vec![0_u8; magic.len()];
    read_exact_at(file, &mut bytes)?;
    if bytes != magic {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid generation lease coordinator magic",
        ));
    }
    Ok(())
}

fn recoverable_coordinator_prefix(file: &File, magic: &[u8]) -> io::Result<bool> {
    verify_private_file_handle(file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.len() >= magic.len() as u64
    {
        return Ok(false);
    }
    let mut bytes = vec![0_u8; usize::try_from(metadata.len()).map_err(|_| invalid_state())?];
    read_exact_at(file, &mut bytes)?;
    Ok(magic.starts_with(&bytes))
}

fn verify_initialization_file(file: &File) -> io::Result<()> {
    verify_private_file_handle(file)?;
    let metadata = file.metadata()?;
    if metadata.is_file()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.nlink() == 1
        && metadata.len() == 0
    {
        Ok(())
    } else {
        Err(invalid_state())
    }
}

fn read_exact_at(file: &File, bytes: &mut [u8]) -> io::Result<()> {
    let mut read = 0_usize;
    while read < bytes.len() {
        let count = file.read_at(&mut bytes[read..], read as u64)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "lease coordinator read",
            ));
        }
        read += count;
    }
    Ok(())
}

fn write_exact_at(file: &File, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0_usize;
    while written < bytes.len() {
        let count = file.write_at(&bytes[written..], written as u64)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "lease coordinator write",
            ));
        }
        written += count;
    }
    Ok(())
}

fn identity(file: &File) -> io::Result<ObjectIdentity> {
    let metadata = file.metadata()?;
    object_identity(metadata.dev(), metadata.ino())
}

fn identity_at(root: &File, name: &CString) -> io::Result<ObjectIdentity> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the descriptor, name, and output pointer remain valid.
    if unsafe {
        libc::fstatat(
            root.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(invalid_state());
    }
    object_identity(metadata.st_dev, metadata.st_ino)
}

fn object_identity(
    device: impl TryInto<u64>,
    inode: impl TryInto<u64>,
) -> io::Result<ObjectIdentity> {
    Ok(ObjectIdentity {
        device: device.try_into().map_err(|_| invalid_state())?,
        inode: inode.try_into().map_err(|_| invalid_state())?,
    })
}

fn set_lock(file: &File, offset: u64, kind: LockKind) -> io::Result<bool> {
    let mut lock = libc::flock {
        l_type: match kind {
            LockKind::Shared => libc::F_RDLCK as _,
            LockKind::Exclusive => libc::F_WRLCK as _,
        },
        l_whence: libc::SEEK_SET as _,
        l_start: i64::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lease range offset"))?,
        l_len: 1,
        l_pid: 0,
    };
    loop {
        // SAFETY: `lock` is initialized and the descriptor remains live.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if matches!(error.raw_os_error(), Some(libc::EACCES | libc::EAGAIN)) {
            return Ok(false);
        }
        return Err(error);
    }
}

fn lock_initialization(file: &File) -> io::Result<()> {
    let mut lock = libc::flock {
        l_type: libc::F_WRLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    loop {
        // SAFETY: `lock` is initialized and the descriptor remains live.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLKW, &mut lock) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn unlock(file: &File, offset: u64) -> io::Result<()> {
    let mut lock = libc::flock {
        l_type: libc::F_UNLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: i64::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lease range offset"))?,
        l_len: 1,
        l_pid: 0,
    };
    loop {
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn path_component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lease coordinator name is not one path component",
        ));
    }
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lease coordinator NUL"))
}

fn invalid_state() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid generation lease coordinator",
    )
}
