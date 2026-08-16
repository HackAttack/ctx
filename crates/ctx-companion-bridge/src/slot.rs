use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use crate::{BridgeError, PairIdentity};

#[cfg(unix)]
#[path = "slot/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "slot/windows.rs"]
mod platform;

pub(crate) use platform::ExecutionBinding;

pub(crate) const MAX_COMPONENT_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const ENVELOPE_RELATIVE_PATH: [&str; 3] = ["share", "ctx", "managed-pair-envelope.json"];
pub(crate) const STATE_RELATIVE_PATH: [&str; 3] = ["share", "ctx", "managed-pair-state.json"];

pub(crate) struct PreparedPair {
    pub(crate) identity: PairIdentity,
    pub(crate) execution: ExecutionBinding,
}

impl PreparedPair {
    pub(crate) fn read_shared_file(
        &self,
        relative: &[&str],
        maximum: usize,
    ) -> Result<Vec<u8>, BridgeError> {
        self.execution.read_owner_safe_file(relative, maximum)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SlotPaths {
    pub(crate) root: PathBuf,
}

impl SlotPaths {
    pub(crate) fn from_launcher(launcher: &Path) -> Result<Self, BridgeError> {
        if !launcher.is_absolute() {
            return Err(BridgeError::InvalidSlot("launcher path is not absolute"));
        }
        if launcher
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(BridgeError::InvalidSlot(
                "launcher path contains a traversal component",
            ));
        }
        if launcher.file_name() != Some(OsStr::new(core_filename())) {
            return Err(BridgeError::InvalidSlot(
                "launcher does not occupy the fixed Core slot",
            ));
        }
        let bin = launcher
            .parent()
            .ok_or(BridgeError::InvalidSlot("launcher has no bin directory"))?;
        if bin.file_name() != Some(OsStr::new("bin")) {
            return Err(BridgeError::InvalidSlot(
                "launcher is not under the fixed bin directory",
            ));
        }
        let root = bin
            .parent()
            .filter(|path| path.parent().is_some())
            .ok_or(BridgeError::InvalidSlot("managed root is missing"))?;
        let root = root.to_path_buf();
        Ok(Self { root })
    }
}

pub(crate) fn prepare(launcher: &Path) -> Result<PreparedPair, BridgeError> {
    let paths = SlotPaths::from_launcher(launcher)?;
    platform::prepare(paths)
}

pub(crate) const fn core_filename() -> &'static str {
    if cfg!(windows) {
        "ctx.exe"
    } else {
        "ctx"
    }
}

pub(crate) const fn companion_filename() -> &'static str {
    if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    }
}
