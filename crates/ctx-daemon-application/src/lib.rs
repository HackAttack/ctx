//! Transport-neutral daemon lifecycle policy.
//!
//! The CLI supplies the concrete installation, endpoint, and host-service
//! operations through [`DaemonApplicationHost`].  This crate deliberately owns
//! neither CLI parsing/rendering nor upgrade-engine authority.

use std::{
    path::{Path, PathBuf},
    process::Child,
    time::Duration,
};

use anyhow::Result;
use ctx_daemon_runtime::NormalizedLaunch;
use serde_json::Value;

mod lifecycle;
mod supervisor;

pub use lifecycle::{
    configured_daemon_autostart_command, configured_unsupervised_daemon_autostart_command,
    daemon_autostart_allowed, daemon_autostart_command, daemon_autostart_suppression_reason,
    daemon_restart_trigger, parse_persisted_trigger, spawn_detached_daemon_child, DaemonHandoff,
    DaemonStartError,
};
pub use supervisor::{
    DaemonSupervisorStart, DaemonSupervisorUpgradeFence, DaemonSupervisorUpgradeResume,
};

/// Narrow boundary for product-specific operations around neutral lifecycle
/// policy. Implementations belong to the CLI composition layer.
pub trait DaemonApplicationHost: Send + Sync {
    fn hosted_uninstall_active(&self) -> Result<bool>;
    fn hosted_uninstall_active_for_executable(&self, executable: &Path) -> Result<bool>;
    fn managed_install_executable(&self) -> Result<Option<PathBuf>>;
    fn installation_upgrade_active(&self) -> Result<bool>;
    fn daemon_config(&self, data_root: &Path) -> Result<DaemonConfigSnapshot>;
    fn daemon_upgrade_handoff_fences_start(&self, data_root: &Path) -> bool;
    fn write_restart_request(&self, data_root: &Path, trigger: DaemonTrigger) -> Result<PathBuf>;
    fn request_lifecycle_wakeup(
        &self,
        data_root: &Path,
        request: Value,
        timeout: Duration,
        response_limit: u64,
    ) -> Result<Option<Value>>;
    fn home_dir(&self) -> Option<PathBuf>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfigSnapshot {
    pub enabled: bool,
    pub mode: DaemonMode,
    pub semantic_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
    Full,
    SourceRefreshOnly,
}

impl DaemonMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SourceRefreshOnly => "source-refresh-only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTrigger {
    Setup,
    Import,
    Search,
}

impl DaemonTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
        }
    }

    pub fn parse_persisted(value: &str) -> Option<Self> {
        match value {
            "setup" => Some(Self::Setup),
            "import" => Some(Self::Import),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonStartMode {
    Auto,
}

impl DaemonStartMode {
    const fn as_str(self) -> &'static str {
        "auto"
    }
}

/// Borrowed application policy facade. Constructing it performs no allocation,
/// lookup, configuration load, or I/O.
#[derive(Clone, Copy)]
pub struct DaemonApplication<'a> {
    host: &'a dyn DaemonApplicationHost,
}

impl<'a> DaemonApplication<'a> {
    pub const fn new(host: &'a dyn DaemonApplicationHost) -> Self {
        Self { host }
    }

    pub fn ensure_daemon_supervisor(&self, data_root: &Path) -> Result<DaemonSupervisorStart> {
        supervisor::ensure_daemon_supervisor(self.host, data_root)
    }

    pub fn disable_daemon_supervisor(&self, data_root: &Path) -> Result<()> {
        supervisor::disable_daemon_supervisor(self.host, data_root)
    }

    pub fn daemon_supervisor_report(&self, data_root: &Path) -> Value {
        supervisor::daemon_supervisor_report(self.host, data_root)
    }

    pub fn resume_daemon_supervisor_after_upgrade(
        &self,
        data_root: &Path,
        executable: &Path,
        upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
    ) -> Result<DaemonSupervisorUpgradeResume> {
        supervisor::resume_daemon_supervisor_after_upgrade(
            self.host,
            data_root,
            executable,
            upgrade_fence,
        )
    }

    pub fn start_daemon_and_wait(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
        bounded_unsupervised: bool,
    ) -> std::result::Result<DaemonHandoff, DaemonStartError> {
        lifecycle::start_daemon_and_wait(
            self.host,
            data_root,
            config,
            trigger,
            bounded_unsupervised,
        )
    }

    pub fn daemon_start_is_fenced(&self) -> bool {
        lifecycle::daemon_start_is_fenced(self.host)
    }

    pub fn request_daemon_start(
        &self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        trigger: DaemonTrigger,
        bounded_unsupervised: bool,
    ) -> Result<()> {
        lifecycle::request_daemon_start(self.host, data_root, config, trigger, bounded_unsupervised)
    }

    pub fn handoff_mismatched_daemon_owner(
        &self,
        data_root: &Path,
        executable: &Path,
    ) -> Result<()> {
        lifecycle::handoff_mismatched_daemon_owner(self.host, data_root, executable)
    }

    pub fn spawn_daemon_child(&self, launch: NormalizedLaunch) -> std::io::Result<Child> {
        lifecycle::spawn_daemon_child(self.host, launch)
    }

    pub fn spawn_daemon_child_for_upgrade_handoff(
        &self,
        launch: NormalizedLaunch,
        executable: &Path,
    ) -> std::io::Result<Child> {
        lifecycle::spawn_daemon_child_for_upgrade_handoff(self.host, launch, executable)
    }

    pub fn daemon_restart_allowed(&self, data_root: &Path) -> Result<bool> {
        lifecycle::daemon_restart_allowed(self.host, data_root)
    }
}

#[cfg(test)]
pub(crate) struct TestHost;

#[cfg(test)]
impl DaemonApplicationHost for TestHost {
    fn hosted_uninstall_active(&self) -> Result<bool> {
        Ok(false)
    }

    fn hosted_uninstall_active_for_executable(&self, _executable: &Path) -> Result<bool> {
        Ok(false)
    }

    fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn installation_upgrade_active(&self) -> Result<bool> {
        Ok(false)
    }

    fn daemon_config(&self, _data_root: &Path) -> Result<DaemonConfigSnapshot> {
        Ok(DaemonConfigSnapshot {
            enabled: true,
            mode: DaemonMode::Full,
            semantic_enabled: true,
        })
    }

    fn daemon_upgrade_handoff_fences_start(&self, _data_root: &Path) -> bool {
        false
    }

    fn write_restart_request(&self, data_root: &Path, _trigger: DaemonTrigger) -> Result<PathBuf> {
        Ok(data_root.to_path_buf())
    }

    fn request_lifecycle_wakeup(
        &self,
        _data_root: &Path,
        _request: Value,
        _timeout: Duration,
        _response_limit: u64,
    ) -> Result<Option<Value>> {
        Ok(None)
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
pub(crate) fn test_environment_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

fn compact_json(mut value: Value) -> Value {
    fn compact(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.retain(|_, value| !value.is_null());
                for value in object.values_mut() {
                    compact(value);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(compact),
            _ => {}
        }
    }
    compact(&mut value);
    value
}
