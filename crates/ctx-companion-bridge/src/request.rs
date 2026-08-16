use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::{environment::CompanionEnvironment, limits::LimitConfiguration, BridgeError};

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct CompanionRequest {
    pub(crate) data_root: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: CompanionEnvironment,
}

impl CompanionRequest {
    /// Creates control data for one companion launch.
    ///
    /// The effective data root is mandatory because the bridge clears the
    /// ambient environment before executing the fixed companion.
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            arguments: Vec::new(),
            environment: CompanionEnvironment::new(),
        }
    }

    pub fn push_argument(&mut self, argument: impl Into<OsString>) -> &mut Self {
        self.arguments.push(argument.into());
        self
    }

    /// Selects bounded request/response transport for machine-oriented calls.
    pub fn capture(self, stdin: impl Into<Vec<u8>>) -> CapturedCompanionRequest {
        CapturedCompanionRequest {
            control: self,
            stdin: stdin.into(),
        }
    }

    pub fn environment_mut(&mut self) -> &mut CompanionEnvironment {
        &mut self.environment
    }

    pub(crate) fn validate(&self, limits: LimitConfiguration) -> Result<(), BridgeError> {
        if self.arguments.len() > limits.arguments {
            return Err(BridgeError::Limit("argument count"));
        }
        if self.environment.len() > limits.environment_entries {
            return Err(BridgeError::Limit("environment entry count"));
        }
        if !self.data_root.is_absolute() {
            return Err(BridgeError::InvalidDataRoot);
        }
        reject_nul(self.data_root.as_os_str())?;
        let mut control_bytes = native_size(self.data_root.as_os_str())
            .checked_add(1)
            .ok_or(BridgeError::Limit("control bytes"))?;
        for argument in &self.arguments {
            control_bytes = control_bytes
                .checked_add(native_size(argument))
                .and_then(|value| value.checked_add(1))
                .ok_or(BridgeError::Limit("control bytes"))?;
            reject_nul(argument)?;
        }
        for (key, value) in self.environment.iter() {
            control_bytes = control_bytes
                .checked_add(key.as_str().len())
                .and_then(|total| total.checked_add(native_size(value)))
                .and_then(|total| total.checked_add(2))
                .ok_or(BridgeError::Limit("control bytes"))?;
            reject_nul(value)?;
        }
        if control_bytes > limits.control_bytes {
            return Err(BridgeError::Limit("control bytes"));
        }
        Ok(())
    }

    pub(crate) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[derive(Clone, Debug)]
pub struct CapturedCompanionRequest {
    pub(crate) control: CompanionRequest,
    pub(crate) stdin: Vec<u8>,
}

impl CapturedCompanionRequest {
    pub(crate) fn validate(&self, limits: LimitConfiguration) -> Result<(), BridgeError> {
        self.control.validate(limits)?;
        if self.stdin.len() > limits.input_bytes {
            return Err(BridgeError::Limit("input bytes"));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn native_size(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().len()
}

#[cfg(windows)]
fn native_size(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().count().saturating_mul(2)
}

#[cfg(not(any(unix, windows)))]
fn native_size(_value: &OsStr) -> usize {
    usize::MAX
}

#[cfg(unix)]
fn reject_nul(value: &OsStr) -> Result<(), BridgeError> {
    use std::os::unix::ffi::OsStrExt as _;
    if value.as_bytes().contains(&0) {
        Err(BridgeError::Limit("control bytes"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn reject_nul(value: &OsStr) -> Result<(), BridgeError> {
    use std::os::windows::ffi::OsStrExt as _;
    if value.encode_wide().any(|unit| unit == 0) {
        Err(BridgeError::Limit("control bytes"))
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn reject_nul(_value: &OsStr) -> Result<(), BridgeError> {
    Err(BridgeError::UnsupportedPlatform)
}
