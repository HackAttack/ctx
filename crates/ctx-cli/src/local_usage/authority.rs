use std::{fs::Metadata, time::SystemTime};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageControlSnapshot {
    enabled: bool,
    available: bool,
    revision: Option<UsageControlRevision>,
}

impl UsageControlSnapshot {
    pub(crate) const fn new(enabled: bool, revision: Option<UsageControlRevision>) -> Self {
        Self {
            enabled,
            available: true,
            revision,
        }
    }

    pub(crate) const fn unavailable(enabled: bool, revision: Option<UsageControlRevision>) -> Self {
        Self {
            enabled,
            available: false,
            revision,
        }
    }

    pub(crate) const fn unversioned(enabled: bool) -> Self {
        Self::new(enabled, None)
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) const fn available(&self) -> bool {
        self.available
    }

    pub(crate) const fn revision(&self) -> Option<&UsageControlRevision> {
        self.revision.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UsageControlRevision {
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
    pub(crate) const fn missing() -> Self {
        Self::Missing
    }

    pub(crate) fn from_file_metadata(metadata: &Metadata) -> Option<Self> {
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
