use std::{
    fs::{File, Metadata},
    path::Path,
};

use sha2::{Digest, Sha256};

use super::JsonlFileObservation;
#[cfg(target_os = "windows")]
use crate::CaptureError;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFileIdentityPolicy {
    SharedV1,
    OrdinaryV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlRetainedFileIdentity {
    stable: [u8; 32],
    change: [u8; 32],
}

impl JsonlRetainedFileIdentity {
    pub(crate) fn stable(self) -> [u8; 32] {
        self.stable
    }

    pub(crate) fn change(self) -> [u8; 32] {
        self.change
    }
}

pub(super) fn observe_metadata(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<JsonlFileObservation> {
    let identity = retained_file_identity(path, file, metadata, JsonlFileIdentityPolicy::SharedV1)?;
    Ok(JsonlFileObservation::new(
        metadata.len(),
        metadata.modified()?,
        metadata.permissions().readonly(),
        identity.map(JsonlRetainedFileIdentity::stable),
        identity.map(JsonlRetainedFileIdentity::change),
    ))
}

#[cfg(unix)]
pub(crate) fn retained_file_identity(
    _path: &Path,
    _file: &File,
    metadata: &Metadata,
    policy: JsonlFileIdentityPolicy,
) -> Result<Option<JsonlRetainedFileIdentity>> {
    use std::os::unix::fs::MetadataExt;

    let mut stable = Sha256::new();
    let mut change = Sha256::new();
    match policy {
        JsonlFileIdentityPolicy::SharedV1 => {
            stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
            change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
        }
        JsonlFileIdentityPolicy::OrdinaryV2 => {
            stable.update(b"ctx-ordinary-file-observation-v2\0unix-stable\0");
            change.update(b"ctx-ordinary-file-observation-v2\0unix-change\0");
        }
    }
    stable.update(metadata.dev().to_le_bytes());
    stable.update(metadata.ino().to_le_bytes());
    if policy == JsonlFileIdentityPolicy::OrdinaryV2 {
        stable.update(metadata.mode().to_le_bytes());
        change.update(metadata.dev().to_le_bytes());
        change.update(metadata.ino().to_le_bytes());
    }
    change.update(metadata.ctime().to_le_bytes());
    change.update(metadata.ctime_nsec().to_le_bytes());
    Ok(Some(JsonlRetainedFileIdentity {
        stable: stable.finalize().into(),
        change: change.finalize().into(),
    }))
}

#[cfg(target_os = "windows")]
pub(crate) fn retained_file_identity(
    path: &Path,
    file: &File,
    metadata: &Metadata,
    policy: JsonlFileIdentityPolicy,
) -> Result<Option<JsonlRetainedFileIdentity>> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic_info = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic_info as *mut FILE_BASIC_INFO as *mut std::ffi::c_void,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if basic_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "reparse-point provider transcript files are rejected",
        });
    }
    let mut id_info = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id_info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut stable = Sha256::new();
    let mut change = Sha256::new();
    match policy {
        JsonlFileIdentityPolicy::SharedV1 => {
            stable.update(b"ctx-jsonl-retained-file-identity-v1\0windows-stable\0");
            change.update(b"ctx-jsonl-retained-file-identity-v1\0windows-change\0");
        }
        JsonlFileIdentityPolicy::OrdinaryV2 => {
            stable.update(b"ctx-ordinary-file-observation-v2\0windows-stable\0");
            change.update(b"ctx-ordinary-file-observation-v2\0windows-change\0");
        }
    }
    stable.update(id_info.VolumeSerialNumber.to_le_bytes());
    stable.update(id_info.FileId.Identifier);
    if policy == JsonlFileIdentityPolicy::OrdinaryV2 {
        stable.update(basic_info.CreationTime.to_le_bytes());
        change.update(id_info.VolumeSerialNumber.to_le_bytes());
        change.update(id_info.FileId.Identifier);
    }
    change.update(basic_info.ChangeTime.to_le_bytes());
    change.update(basic_info.LastWriteTime.to_le_bytes());
    match policy {
        JsonlFileIdentityPolicy::SharedV1 => {
            change.update(basic_info.FileAttributes.to_le_bytes());
        }
        JsonlFileIdentityPolicy::OrdinaryV2 => change.update(metadata.len().to_le_bytes()),
    }
    Ok(Some(JsonlRetainedFileIdentity {
        stable: stable.finalize().into(),
        change: change.finalize().into(),
    }))
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn retained_file_identity(
    _path: &Path,
    _file: &File,
    _metadata: &Metadata,
    _policy: JsonlFileIdentityPolicy,
) -> Result<Option<JsonlRetainedFileIdentity>> {
    Ok(None)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn policies_preserve_shared_and_codex_identity_domains() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();

        let shared =
            retained_file_identity(&path, &file, &metadata, JsonlFileIdentityPolicy::SharedV1)
                .unwrap()
                .unwrap();
        let mut shared_stable = Sha256::new();
        shared_stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
        shared_stable.update(metadata.dev().to_le_bytes());
        shared_stable.update(metadata.ino().to_le_bytes());
        let mut shared_change = Sha256::new();
        shared_change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
        shared_change.update(metadata.ctime().to_le_bytes());
        shared_change.update(metadata.ctime_nsec().to_le_bytes());
        let expected_shared_stable: [u8; 32] = shared_stable.finalize().into();
        let expected_shared_change: [u8; 32] = shared_change.finalize().into();
        assert_eq!(shared.stable(), expected_shared_stable);
        assert_eq!(shared.change(), expected_shared_change);

        let ordinary =
            retained_file_identity(&path, &file, &metadata, JsonlFileIdentityPolicy::OrdinaryV2)
                .unwrap()
                .unwrap();
        let mut ordinary_stable = Sha256::new();
        ordinary_stable.update(b"ctx-ordinary-file-observation-v2\0unix-stable\0");
        ordinary_stable.update(metadata.dev().to_le_bytes());
        ordinary_stable.update(metadata.ino().to_le_bytes());
        ordinary_stable.update(metadata.mode().to_le_bytes());
        let mut ordinary_change = Sha256::new();
        ordinary_change.update(b"ctx-ordinary-file-observation-v2\0unix-change\0");
        ordinary_change.update(metadata.dev().to_le_bytes());
        ordinary_change.update(metadata.ino().to_le_bytes());
        ordinary_change.update(metadata.ctime().to_le_bytes());
        ordinary_change.update(metadata.ctime_nsec().to_le_bytes());
        let expected_ordinary_stable: [u8; 32] = ordinary_stable.finalize().into();
        let expected_ordinary_change: [u8; 32] = ordinary_change.finalize().into();
        assert_eq!(ordinary.stable(), expected_ordinary_stable);
        assert_eq!(ordinary.change(), expected_ordinary_change);
    }
}
