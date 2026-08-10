use std::{
    fs::Metadata,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalUsageStorageAuthority {
    database_path: PathBuf,
    product_version: &'static str,
}

impl LocalUsageStorageAuthority {
    pub fn new(database_path: PathBuf, product_version: &'static str) -> Self {
        Self {
            database_path,
            product_version,
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn product_version(&self) -> &'static str {
        self.product_version
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageControlSnapshot {
    enabled: bool,
    available: bool,
    revision: Option<UsageControlRevision>,
}

impl UsageControlSnapshot {
    pub const fn new(enabled: bool, revision: Option<UsageControlRevision>) -> Self {
        Self {
            enabled,
            available: true,
            revision,
        }
    }

    pub const fn unavailable(enabled: bool, revision: Option<UsageControlRevision>) -> Self {
        Self {
            enabled,
            available: false,
            revision,
        }
    }

    pub const fn unversioned(enabled: bool) -> Self {
        Self::new(enabled, None)
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn available(&self) -> bool {
        self.available
    }

    pub const fn revision(&self) -> Option<&UsageControlRevision> {
        self.revision.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageControlRevision {
    Missing,
    File {
        len: u64,
        modified: SystemTime,
        created: Option<SystemTime>,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        changed_seconds: i64,
        #[cfg(unix)]
        changed_nanoseconds: i64,
    },
}

impl UsageControlRevision {
    pub const fn missing() -> Self {
        Self::Missing
    }

    pub fn from_file_metadata(metadata: &Metadata) -> Option<Self> {
        if !metadata.is_file() {
            return None;
        }
        Some(Self::File {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}
