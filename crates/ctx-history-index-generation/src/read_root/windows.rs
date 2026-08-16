use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt as _,
        fs::OpenOptionsExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, RawHandle},
    },
    path::{Component, Path, PathBuf},
};

use ctx_history_platform::platform_security::verify_private_directory_handle;
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
        Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::DirectoryIdentity;

#[derive(Debug)]
pub(crate) struct OpenedDirectory {
    route: Vec<File>,
    file: File,
    identity: ObjectIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    volume: u64,
    file: u64,
}

impl OpenedDirectory {
    pub(crate) fn open_absolute(path: &Path) -> io::Result<Self> {
        let (root_path, components) = absolute_components(path)?;
        let root = OpenOptions::new()
            .access_mode(FILE_GENERIC_READ | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(root_path)?;
        verify_directory(&root)?;
        let mut opened = Self::from_file(Vec::new(), root)?;
        for component in components {
            opened = opened.open_component(&component)?;
        }
        Ok(opened)
    }

    pub(crate) fn open_directory(&self, relative: &Path) -> io::Result<Self> {
        let mut opened = Self::from_file(
            self.route
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<Vec<_>>>()?,
            self.file.try_clone()?,
        )?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            opened = opened.open_component(name)?;
        }
        Ok(opened)
    }

    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<File> {
        let mut components = relative.components().peekable();
        let mut parent = self.file.try_clone()?;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid_path());
            };
            if components.peek().is_some() {
                parent = nt_open_at(&parent, name, FILE_DIRECTORY_FILE)?;
                verify_directory(&parent)?;
                continue;
            }
            let file = nt_open_at(&parent, name, FILE_NON_DIRECTORY_FILE)?;
            let information = information(&file)?;
            if information.dwFileAttributes
                & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                != 0
            {
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
        verify_private_directory_handle(&self.file)
    }

    pub(crate) fn verify_lease_root(&self) -> io::Result<()> {
        self.verify_private()
    }

    pub(crate) fn stable_path(&self, original_path: &Path) -> io::Result<PathBuf> {
        let reopened = Self::open_absolute(original_path)?;
        if reopened.identity != self.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "generation read pathname changed identity",
            ));
        }
        Ok(original_path.to_path_buf())
    }

    pub(crate) fn try_clone_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    pub(crate) fn registry_identity(&self) -> DirectoryIdentity {
        DirectoryIdentity::Windows {
            volume: self.identity.volume,
            file: self.identity.file,
        }
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn open_component(mut self, name: &OsStr) -> io::Result<Self> {
        let next = nt_open_at(&self.file, name, FILE_DIRECTORY_FILE)?;
        verify_directory(&next)?;
        self.route.push(self.file);
        Self::from_file(self.route, next)
    }

    fn from_file(route: Vec<File>, file: File) -> io::Result<Self> {
        verify_directory(&file)?;
        let identity = identity(&file)?;
        Ok(Self {
            route,
            file,
            identity,
        })
    }
}

fn absolute_components(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(invalid_path());
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(invalid_path());
    }
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push("\\");
    let mut names = Vec::new();
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid_path());
        };
        names.push(name.to_os_string());
    }
    Ok((root, names))
}

fn nt_open_at(parent: &File, name: &OsStr, create_options: u32) -> io::Result<File> {
    let mut wide = name.encode_wide().collect::<Vec<_>>();
    if wide.is_empty()
        || wide
            .iter()
            .any(|unit| *unit == 0 || *unit == b'/' as u16 || *unit == b'\\' as u16)
    {
        return Err(invalid_path());
    }
    let byte_len = wide
        .len()
        .checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(invalid_path)?;
    let mut unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &mut unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ | READ_CONTROL | SYNCHRONIZE,
            &object_attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            create_options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

fn verify_directory(file: &File) -> io::Result<()> {
    let information = information(file)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
        && information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generation read directory is a reparse point or non-directory",
        ))
    }
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { information.assume_init() })
    }
}

fn identity(file: &File) -> io::Result<ObjectIdentity> {
    let information = information(file)?;
    Ok(ObjectIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        file: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

fn invalid_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "generation read path must be absolute and traversal-free",
    )
}
