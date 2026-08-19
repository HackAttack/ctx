use std::{
    ffi::{CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    path::{Component, Path, PathBuf},
};

use super::DirectoryIdentity;

#[derive(Debug)]
pub(crate) struct OpenedDirectory {
    file: File,
    identity: ObjectIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
}

impl OpenedDirectory {
    pub(crate) fn open_absolute(path: &Path) -> io::Result<Self> {
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::RootDir)) {
            return Err(invalid_path());
        }
        let root_name = CString::new("/").expect("root has no NUL");
        let descriptor = unsafe {
            libc::open(
                root_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut current = unsafe { File::from_raw_fd(descriptor) };
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            current = open_directory_at(current.as_raw_fd(), name)?;
        }
        Self::from_file(current)
    }

    pub(crate) fn open_directory(&self, relative: &Path) -> io::Result<Self> {
        let mut current = self.file.try_clone()?;
        let mut saw_component = false;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            saw_component = true;
            current = open_directory_at(current.as_raw_fd(), name)?;
        }
        if !saw_component {
            return Self::from_file(current);
        }
        Self::from_file(current)
    }

    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<File> {
        let mut components = relative.components().peekable();
        let mut parent = self.file.try_clone()?;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            if components.peek().is_some() {
                parent = open_directory_at(parent.as_raw_fd(), name)?;
                continue;
            }
            let name = component_name(name)?;
            let descriptor = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                )
            };
            if descriptor < 0 {
                return Err(io::Error::last_os_error());
            }
            let file = unsafe { File::from_raw_fd(descriptor) };
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "anchored generation target is not a regular file",
                ));
            }
            return Ok(file);
        }
        Err(invalid_path())
    }

    pub(crate) fn verify_private(&self) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        if metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "generation read directory is not owner-private",
            ))
        }
    }

    pub(crate) fn verify_lease_root(&self) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        if metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "generation lease root is not owned and protected from writes",
            ))
        }
    }

    pub(crate) fn stable_path(&self, original_path: &Path) -> io::Result<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            // Darwin's fdesc nodes duplicate a directory descriptor when
            // opened, but cannot be traversed as `/dev/fd/<n>/child`. Keep a
            // usable registry key after binding it to the retained descriptor;
            // registered descendant access still uses that descriptor.
            let reopened = Self::open_absolute(original_path)?;
            if reopened.identity != self.identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stable directory-handle path changed identity",
                ));
            }
            Ok(original_path.to_path_buf())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = original_path;
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let path = PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let path = PathBuf::from(format!("/dev/fd/{}", self.file.as_raw_fd()));
            let metadata = std::fs::metadata(&path)?;
            let observed = ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            };
            if observed != self.identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stable directory-handle path changed identity",
                ));
            }
            Ok(path)
        }
    }

    pub(crate) fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    pub(crate) fn registry_identity(&self) -> DirectoryIdentity {
        DirectoryIdentity::Unix {
            device: self.identity.device,
            inode: self.identity.inode,
        }
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn from_file(file: File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "anchored generation target is not a directory",
            ));
        }
        let identity = identity(&file)?;
        Ok(Self { file, identity })
    }
}

fn open_directory_at(parent: libc::c_int, name: &OsStr) -> io::Result<File> {
    let name = component_name(name)?;
    let descriptor = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn component_name(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(invalid_path());
    }
    CString::new(name.as_bytes()).map_err(|_| invalid_path())
}

fn identity(file: &File) -> io::Result<ObjectIdentity> {
    let metadata = file.metadata()?;
    Ok(ObjectIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn invalid_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "generation read path must be absolute and traversal-free",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_path_is_normalized_and_uses_the_opened_directory_identity() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let original = directory.path().canonicalize()?;
        let opened = OpenedDirectory::open_absolute(&original)?;
        std::fs::write(original.join("child"), b"stable child")?;

        let path = opened.stable_path(&original)?;

        assert!(path.is_absolute());
        assert!(!path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir)));
        let metadata = std::fs::metadata(&path)?;
        assert_eq!(metadata.dev(), opened.identity.device);
        assert_eq!(metadata.ino(), opened.identity.inode);
        assert_eq!(std::fs::read(path.join("child"))?, b"stable child");
        Ok(())
    }
}
