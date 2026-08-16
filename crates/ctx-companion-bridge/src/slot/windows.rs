use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read as _, Seek as _, SeekFrom},
    os::windows::{
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::AsRawHandle as _,
    },
    path::{Component, Path, PathBuf},
};

use ctx_history_platform::platform_security::{
    verify_private_directory_handle, verify_private_file_handle,
};
use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, READ_CONTROL,
};

use crate::{
    identity::{FileIdentity, PairIdentity, Sha256Digest},
    BridgeError,
};

use super::{PreparedPair, SlotPaths, MAX_COMPONENT_BYTES};

pub(crate) struct ExecutionBinding {
    root_path: PathBuf,
    root: File,
    _bin: File,
    _libexec: File,
    executable: File,
}

impl ExecutionBinding {
    pub(crate) fn program(&self) -> PathBuf {
        self.root_path
            .join("libexec")
            .join(super::companion_filename())
    }

    pub(crate) fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub(crate) fn verify_retained(&self) -> Result<(), BridgeError> {
        verify_private_directory_handle(&self.root)
            .map_err(|error| BridgeError::filesystem("verify retained managed root", error))?;
        verify_private_file_handle(&self.executable)
            .map_err(|error| BridgeError::filesystem("verify retained companion handle", error))
    }

    pub(crate) fn read_owner_safe_file(
        &self,
        relative: &[&str],
        maximum: usize,
    ) -> Result<Vec<u8>, BridgeError> {
        if relative.len() < 2 {
            return Err(BridgeError::InvalidSlot("shared path is incomplete"));
        }
        let mut path = self.root_path.clone();
        let mut directories = Vec::new();
        for component in &relative[..relative.len() - 1] {
            path.push(component);
            let directory = open_directory(&path).map_err(|error| {
                BridgeError::filesystem("open owner-safe shared directory", error)
            })?;
            validate_not_reparse(&directory, true).map_err(|error| {
                BridgeError::filesystem("reject shared-directory reparse point", error)
            })?;
            verify_private_directory_handle(&directory).map_err(|error| {
                BridgeError::filesystem("verify owner-safe shared directory", error)
            })?;
            directories.push(directory);
        }
        path.push(relative[relative.len() - 1]);
        let file = open_file(&path)
            .map_err(|error| BridgeError::filesystem("open owner-safe shared file", error))?;
        validate_not_reparse(&file, false)
            .map_err(|error| BridgeError::filesystem("reject shared-file reparse point", error))?;
        verify_private_file_handle(&file)
            .map_err(|error| BridgeError::filesystem("verify owner-safe shared file", error))?;
        let _held_directories = directories;
        read_bounded(&file, maximum)
            .map_err(|error| BridgeError::filesystem("read owner-safe shared file", error))
    }
}

pub(super) fn prepare(paths: SlotPaths) -> Result<PreparedPair, BridgeError> {
    reject_unsupported_components(&paths.root)?;
    let root = open_directory(&paths.root).map_err(|error| {
        BridgeError::filesystem("open managed root without reparse traversal", error)
    })?;
    let bin = open_directory(&paths.root.join("bin"))
        .map_err(|error| BridgeError::filesystem("open fixed bin directory", error))?;
    let libexec = open_directory(&paths.root.join("libexec"))
        .map_err(|error| BridgeError::filesystem("open fixed libexec directory", error))?;
    for directory in [&root, &bin, &libexec] {
        validate_not_reparse(directory, true).map_err(|error| {
            BridgeError::filesystem("reject managed-directory reparse point", error)
        })?;
        verify_private_directory_handle(directory)
            .map_err(|error| BridgeError::filesystem("verify managed-directory ACL", error))?;
    }
    let launcher = open_file(&paths.root.join("bin").join(super::core_filename()))
        .map_err(|error| BridgeError::filesystem("open fixed Core executable", error))?;
    let companion = open_file(&paths.root.join("libexec").join(super::companion_filename()))
        .map_err(|error| BridgeError::filesystem("open fixed companion executable", error))?;
    for executable in [&launcher, &companion] {
        validate_not_reparse(executable, false)
            .map_err(|error| BridgeError::filesystem("reject executable reparse point", error))?;
        verify_private_file_handle(executable)
            .map_err(|error| BridgeError::filesystem("verify managed executable ACL", error))?;
    }
    let launcher_digest = digest_file(&launcher, MAX_COMPONENT_BYTES)
        .map_err(|error| BridgeError::filesystem("stream-hash fixed Core executable", error))?;
    let companion_digest = digest_file(&companion, MAX_COMPONENT_BYTES).map_err(|error| {
        BridgeError::filesystem("stream-hash fixed companion executable", error)
    })?;
    let identity = PairIdentity::new(
        paths.root.clone(),
        windows_identity(&launcher, launcher_digest)
            .map_err(|error| BridgeError::filesystem("bind fixed Core file identity", error))?,
        windows_identity(&companion, companion_digest).map_err(|error| {
            BridgeError::filesystem("bind fixed companion file identity", error)
        })?,
    );
    Ok(PreparedPair {
        execution: ExecutionBinding {
            root_path: paths.root.clone(),
            root,
            _bin: bin,
            _libexec: libexec,
            executable: companion,
        },
        identity,
    })
}

fn reject_unsupported_components(path: &Path) -> Result<(), BridgeError> {
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        Err(BridgeError::InvalidSlot("managed root contains traversal"))
    } else {
        Ok(())
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn open_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_GENERIC_READ | READ_CONTROL)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn validate_not_reparse(file: &File, directory: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed path has unsafe type",
        ))
    } else {
        Ok(())
    }
}

fn windows_identity(file: &File, digest: Sha256Digest) -> io::Result<FileIdentity> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(FileIdentity::windows(
        file.metadata()?.file_size(),
        digest,
        information.dwVolumeSerialNumber,
        file_index,
    ))
}

fn digest_file(file: &File, maximum: u64) -> io::Result<Sha256Digest> {
    let metadata = file.metadata()?;
    if metadata.file_size() == 0 || metadata.file_size() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "component is outside size bounds",
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
    if total != metadata.file_size() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "component changed while hashing",
        ));
    }
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

fn read_bounded(file: &File, maximum: usize) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if metadata.file_size() == 0 || metadata.file_size() > maximum as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed file is outside size bounds",
        ));
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(metadata.file_size() as usize);
    reader.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum || bytes.len() as u64 != metadata.file_size() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "managed file changed while reading",
        ));
    }
    Ok(bytes)
}
