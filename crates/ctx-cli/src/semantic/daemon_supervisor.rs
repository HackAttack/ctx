use anyhow::{anyhow, Context, Result};
use ctx_daemon_runtime::{
    daemon_lock_is_active, ensure_native_supervisor_with as ensure_runtime_supervisor_with,
    resume_native_supervisor_with as resume_runtime_supervisor_with, NativeSupervisorBackend,
    SupervisorEnsureOutcome, SupervisorIdentity, SupervisorInstallationLock,
    SupervisorManagerEnvironment, SupervisorResumeOutcome, SupervisorSpec, SupervisorUpgradeFence,
};
#[cfg(test)]
use ctx_daemon_runtime::{
    launchctl_print_pid, supervisor_command, systemd_main_pid, verify_daemon_owner_identity,
    write_atomic_supervisor_file as write_atomic_file,
};
use ctx_history_core::managed_data_root;
use serde_json::{json, Value};
use std::env;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[cfg(test)]
use std::{fs, process::Command};

use crate::compact_json;
#[cfg(unix)]
use crate::identity;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use super::query_service::daemon_source_refresh_request;

mod environment;
mod report;
mod state;
#[cfg(test)]
mod tests;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(test)]
mod windows;

#[cfg(all(test, windows))]
use environment::validated_supervisor_artifact_path;
use environment::{
    supervisor_environment_contract_report, supervisor_environment_snapshot,
    SupervisorEnvironmentSnapshot,
};
pub(super) use report::daemon_supervisor_report;
#[cfg(test)]
use report::daemon_supervisor_report_with_normalized_environment;
#[cfg(any(test, target_os = "freebsd"))]
use report::freebsd_supervisor_authority_blocker;
use report::native_supervisor_product_authority_blocker;
#[cfg(test)]
use report::revalidated_supervisor_report_with;
use state::{
    native_supervisor_kind, native_supervisor_limitation, stored_supervisor_report,
    write_installed_receipt, write_supervisor_receipt,
    write_supervisor_receipt_with_environment_snapshot, SupervisorReceipt,
};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported::*;
#[cfg(test)]
use windows::*;

const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
const SUPERVISOR_ENV_ALLOWLIST: &[&str] = &[
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HOME",
    "LANG",
    "LC_ALL",
    "PATH",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USER",
    "USERPROFILE",
    "WAYLAND_DISPLAY",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
];

fn supervisor_manager_environment() -> Result<SupervisorManagerEnvironment> {
    let values = SUPERVISOR_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect::<BTreeMap<_, _>>();
    #[cfg(unix)]
    let values = {
        let mut values = values;
        if !values.contains_key(OsStr::new("HOME")) {
            if let Some(home) = identity::home_dir() {
                values.insert(OsString::from("HOME"), home.into_os_string());
            }
        }
        values
    };
    normalized_supervisor_manager_environment(values)
}

fn normalized_supervisor_manager_environment(
    values: BTreeMap<OsString, OsString>,
) -> Result<SupervisorManagerEnvironment> {
    if let Some(name) = values
        .keys()
        .find(|name| release_authority_environment_name(name.as_os_str()))
    {
        return Err(anyhow!(
            "supervisor manager environment may not contain release authority variable {}",
            name.to_string_lossy()
        ));
    }
    Ok(SupervisorManagerEnvironment::new(values))
}

fn release_authority_environment_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("CTX_RELEASE_") || name == "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL"
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum DaemonSupervisorStart {
    Native,
    Fallback,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum DaemonSupervisorUpgradeResume {
    Native,
    Fallback,
}

pub(in crate::semantic) trait DaemonSupervisorUpgradeFence {
    fn release(&mut self) -> Result<()>;
}

struct RuntimeSupervisorUpgradeFence<'a>(&'a mut dyn DaemonSupervisorUpgradeFence);

impl SupervisorUpgradeFence for RuntimeSupervisorUpgradeFence<'_> {
    fn release(&mut self) -> Result<()> {
        self.0.release()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedSupervisorInput {
    data_root: PathBuf,
    executable: PathBuf,
    daemon_environment: SupervisorEnvironmentSnapshot,
    manager_environment: SupervisorManagerEnvironment,
}

impl ManagedSupervisorInput {
    fn new(data_root: &Path, executable: &Path) -> Result<Self> {
        Ok(Self {
            data_root: data_root.to_path_buf(),
            executable: executable.to_path_buf(),
            daemon_environment: supervisor_environment_snapshot()
                .context("capture native supervisor daemon environment")?,
            manager_environment: supervisor_manager_environment()?,
        })
    }
}

struct PlatformNativeSupervisor<'a> {
    identity: Option<SupervisorIdentity>,
    data_root: PathBuf,
    daemon_environment: Option<&'a SupervisorEnvironmentSnapshot>,
    manager_environment: &'a SupervisorManagerEnvironment,
}

impl<'a> PlatformNativeSupervisor<'a> {
    fn new(
        data_root: &Path,
        daemon_environment: Option<&'a SupervisorEnvironmentSnapshot>,
        manager_environment: &'a SupervisorManagerEnvironment,
    ) -> Result<Self> {
        let identity = native_supervisor_identity(data_root, manager_environment)?;
        Ok(Self {
            identity,
            data_root: data_root.to_path_buf(),
            daemon_environment,
            manager_environment,
        })
    }

    fn identity(&self) -> Result<&SupervisorIdentity> {
        self.identity
            .as_ref()
            .ok_or_else(|| anyhow!("native supervisor identity is unavailable"))
    }

    fn spec(&self, executable: &Path) -> Result<SupervisorSpec> {
        environment::supervisor_artifact_spec(
            self.identity()?.clone(),
            executable,
            &self.data_root,
            self.launch_environment()?,
        )
    }

    fn launch_environment(&self) -> Result<&SupervisorEnvironmentSnapshot> {
        self.daemon_environment.ok_or_else(|| {
            anyhow!(
                "native supervisor launch or verification requires a normalized daemon environment"
            )
        })
    }
}

impl NativeSupervisorBackend<SupervisorEnvironmentSnapshot> for PlatformNativeSupervisor<'_> {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        let _ = data_root;
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        return Ok(None);
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        Ok(self
            .identity
            .as_ref()
            .map(|identity| identity.artifact_path().to_path_buf()))
    }

    fn install(
        &self,
        data_root: &Path,
        executable: &Path,
        environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf> {
        let daemon_environment = self.launch_environment()?;
        debug_assert_eq!(
            environment.values, daemon_environment.values,
            "installation must use the normalized daemon environment"
        );
        let spec = self.spec(executable)?;
        install_native_supervisor(
            data_root,
            executable,
            daemon_environment,
            self.manager_environment,
            &spec,
        )
    }

    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        disable_native_supervisor(data_root, self.manager_environment, self.identity()?)
    }

    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()> {
        let spec = self.spec(executable)?;
        verify_native_supervisor_registration(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
            &spec,
        )
    }

    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        let spec = self.spec(executable)?;
        verify_native_supervisor(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
            &spec,
        )
    }

    fn start(&self, data_root: &Path) -> Result<()> {
        start_native_supervisor(data_root, self.manager_environment, self.identity()?)
    }
}

pub(super) fn ensure_daemon_supervisor(data_root: &Path) -> Result<DaemonSupervisorStart> {
    ensure_hosted_uninstall_supervisor_admission()?;
    let Some(input) = managed_supervisor_input(data_root)? else {
        let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
        ensure_hosted_uninstall_supervisor_admission()?;
        write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: "cli_self_heal".to_owned(),
                status: "fallback",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: None,
                limitation: Some(
                    "native per-user restart registration requires the hosted installer and the default data root"
                        .to_owned(),
                ),
                last_error: None,
            },
        )?;
        return Ok(DaemonSupervisorStart::Fallback);
    };
    super::daemon_autostart::handoff_mismatched_daemon_owner(data_root, &input.executable)
        .context("replace daemon ownership held by a different ctx binary image")?;
    let backend = PlatformNativeSupervisor::new(
        data_root,
        Some(&input.daemon_environment),
        &input.manager_environment,
    )?;
    ensure_native_supervisor_with(&input, &backend)
}

fn managed_supervisor_input(data_root: &Path) -> Result<Option<ManagedSupervisorInput>> {
    safely_supported_managed_install(data_root)?
        .map(|executable| ManagedSupervisorInput::new(data_root, &executable))
        .transpose()
}

fn ensure_native_supervisor_with(
    input: &ManagedSupervisorInput,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
) -> Result<DaemonSupervisorStart> {
    let data_root = input.data_root.as_path();
    let executable = input.executable.as_path();
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    // The uninstall path disables supervisor state under this same lock. Once
    // admitted here, every artifact, manager, start, and receipt mutation below
    // remains serialized ahead of that disable.
    ensure_hosted_uninstall_supervisor_admission()?;
    match ensure_runtime_supervisor_with(data_root, executable, &input.daemon_environment, backend)?
    {
        SupervisorEnsureOutcome::Native {
            artifact,
            owner_pid,
            environment_installed,
        } => {
            write_installed_receipt(
                data_root,
                executable,
                artifact,
                owner_pid,
                environment_installed.then(|| input.daemon_environment.contract_report()),
            )?;
            Ok(DaemonSupervisorStart::Native)
        }
        SupervisorEnsureOutcome::RegisteredNotRunning {
            artifact,
            initial_error,
            recovery_error,
            environment_installed,
        } => {
            let (limitation, last_error) = if environment_installed {
                (
                    "native registration survived installation recovery but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing",
                    format!(
                        "installation: {initial_error:#}; recovery: {recovery_error:#}"
                    ),
                )
            } else {
                (
                    "native registration is valid but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing",
                    format!(
                        "initial live check: {initial_error:#}; recovery: {recovery_error:#}"
                    ),
                )
            };
            let receipt = SupervisorReceipt {
                kind: native_supervisor_kind().to_owned(),
                status: "registered_not_running",
                autostart_supported: true,
                restart_supported: true,
                registration_verified: true,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: artifact,
                executable_path: Some(executable.to_path_buf()),
                limitation: Some(limitation.to_owned()),
                last_error: Some(last_error),
            };
            if environment_installed {
                write_supervisor_receipt_with_environment_snapshot(
                    data_root,
                    &receipt,
                    Some(input.daemon_environment.contract_report()),
                )?;
            } else {
                write_supervisor_receipt(data_root, &receipt)?;
            }
            Ok(DaemonSupervisorStart::Fallback)
        }
        SupervisorEnsureOutcome::InstallFailed {
            artifact,
            error,
            cleanup_error,
        } => {
            if let Some(cleanup_error) = cleanup_error {
                write_supervisor_receipt(
                    data_root,
                    &SupervisorReceipt {
                        kind: native_supervisor_kind().to_owned(),
                        status: "install_cleanup_failed",
                        autostart_supported: false,
                        restart_supported: false,
                        registration_verified: false,
                        live_owner_verified: false,
                        owner_pid: None,
                        artifact_path: artifact,
                        executable_path: Some(executable.to_path_buf()),
                        limitation: Some(
                            "native registration failed and its partial state could not be removed"
                                .to_owned(),
                        ),
                        last_error: Some(format!("{error:#}; cleanup: {cleanup_error:#}")),
                    },
                )?;
                return Err(error.context(format!(
                    "also failed to remove partial native supervisor state: {cleanup_error:#}"
                )));
            }
            let authority_blocker = native_supervisor_product_authority_blocker();
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: if authority_blocker {
                        native_supervisor_kind()
                    } else {
                        "cli_self_heal"
                    }
                    .to_owned(),
                    status: if authority_blocker {
                        "degraded"
                    } else {
                        "install_failed"
                    },
                    autostart_supported: false,
                    restart_supported: false,
                    registration_verified: false,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: None,
                    executable_path: Some(executable.to_path_buf()),
                    limitation: Some(native_supervisor_limitation().to_owned()),
                    last_error: Some(format!("{error:#}")),
                },
            )?;
            if authority_blocker {
                Ok(DaemonSupervisorStart::Fallback)
            } else {
                Err(error).context("install and verify native per-user ctx daemon supervisor")
            }
        }
    }
}

fn ensure_hosted_uninstall_supervisor_admission() -> Result<()> {
    if ctx_upgrade_engine::installation_hosted_uninstall_is_active().unwrap_or(true) {
        return Err(anyhow!(
            "ctx daemon supervisor mutation is fenced by hosted uninstall"
        ));
    }
    Ok(())
}

fn ensure_hosted_uninstall_supervisor_admission_for_executable(executable: &Path) -> Result<()> {
    if ctx_upgrade_engine::installation_hosted_uninstall_is_active_for_executable(executable)
        .unwrap_or(true)
    {
        return Err(anyhow!(
            "ctx daemon supervisor mutation is fenced by hosted uninstall"
        ));
    }
    Ok(())
}

pub(super) fn disable_daemon_supervisor(data_root: &Path) -> Result<()> {
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    let current = stored_supervisor_report(data_root);
    if !is_canonical_managed_data_root(data_root)? {
        return write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: "cli_self_heal".to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: None,
                limitation: Some(
                    "custom data roots never own or alter the singleton native supervisor"
                        .to_owned(),
                ),
                last_error: None,
            },
        );
    }
    let managed_executable = safely_supported_managed_install(data_root)?;
    let executable = current
        .get("executable_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| managed_executable.clone());
    let receipt_native =
        current.get("kind").and_then(Value::as_str) == Some(native_supervisor_kind());
    let native_candidate = receipt_native || managed_executable.is_some();
    if !native_candidate {
        return write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: current
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("cli_self_heal")
                    .to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: executable,
                limitation: None,
                last_error: None,
            },
        );
    }
    let manager_environment = supervisor_manager_environment()?;
    let backend = PlatformNativeSupervisor::new(data_root, None, &manager_environment)?;
    disable_native_supervisor_candidate_with(data_root, executable, &backend)
}

fn disable_native_supervisor_candidate_with(
    data_root: &Path,
    executable: Option<PathBuf>,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
) -> Result<()> {
    // A disable request is idempotent control-plane work. Do not probe through
    // launch verification first: a surviving service-manager registration must
    // still be removed when its launch artifact or launch environment is gone.
    let artifact = backend.artifact_path(data_root).ok().flatten();
    let result = backend.disable(data_root);
    match result {
        Ok(artifact) => write_supervisor_receipt(
            data_root,
            &SupervisorReceipt {
                kind: native_supervisor_kind().to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: artifact,
                executable_path: executable,
                limitation: None,
                last_error: None,
            },
        ),
        Err(error) => {
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: native_supervisor_kind().to_owned(),
                    status: "disable_failed",
                    autostart_supported: false,
                    restart_supported: false,
                    registration_verified: false,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: artifact,
                    executable_path: executable,
                    limitation: Some(
                        "native per-user registration could not be fully removed".to_owned(),
                    ),
                    last_error: Some(format!("{error:#}")),
                },
            )?;
            Err(error)
        }
    }
}

pub(super) fn resume_daemon_supervisor_after_upgrade(
    data_root: &Path,
    executable: &Path,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    let daemon_environment = supervisor_environment_snapshot()
        .context("capture native supervisor daemon environment")?;
    let manager_environment = supervisor_manager_environment()?;
    let backend =
        PlatformNativeSupervisor::new(data_root, Some(&daemon_environment), &manager_environment)?;
    resume_daemon_supervisor_after_upgrade_with(data_root, executable, &backend, upgrade_fence)
}

fn resume_daemon_supervisor_after_upgrade_with(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    ensure_hosted_uninstall_supervisor_admission_for_executable(executable)?;
    let mut runtime_fence = RuntimeSupervisorUpgradeFence(upgrade_fence);
    match resume_runtime_supervisor_with(data_root, executable, backend, &mut runtime_fence)? {
        SupervisorResumeOutcome::Fallback => Ok(DaemonSupervisorUpgradeResume::Fallback),
        SupervisorResumeOutcome::Native {
            artifact,
            owner_pid,
        } => {
            write_installed_receipt(data_root, executable, artifact, owner_pid, None)?;
            Ok(DaemonSupervisorUpgradeResume::Native)
        }
        SupervisorResumeOutcome::RegisteredNotRunning { artifact, error } => {
            write_supervisor_receipt(
                data_root,
                &SupervisorReceipt {
                    kind: native_supervisor_kind().to_owned(),
                    status: "registered_not_running",
                    autostart_supported: true,
                    restart_supported: true,
                    registration_verified: true,
                    live_owner_verified: false,
                    owner_pid: None,
                    artifact_path: artifact,
                    executable_path: Some(executable.to_path_buf()),
                    limitation: Some(
                        "the upgrade fence was released to a valid native registration, but the manager did not establish identity-verified daemon ownership; the durable restart request remains available for CLI self-healing"
                            .to_owned(),
                    ),
                    last_error: Some(format!("{error:#}")),
                },
            )?;
            Err(error).context("return upgraded daemon lifecycle ownership to native supervisor")
        }
    }
}

fn safely_supported_managed_install(data_root: &Path) -> Result<Option<PathBuf>> {
    if !is_canonical_managed_data_root(data_root)? {
        return Ok(None);
    }
    ctx_upgrade_engine::managed_install_executable()
}

fn is_canonical_managed_data_root(data_root: &Path) -> Result<bool> {
    let managed_root = managed_data_root().context("resolve canonical managed ctx data root")?;
    Ok(data_root == managed_root)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn migrate_existing_daemon_to_supervisor(data_root: &Path) -> Result<()> {
    if !daemon_lock_is_active(data_root) {
        return Ok(());
    }
    let response = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "supervisor_handoff",
        })),
        Duration::from_millis(500),
        16 * 1024,
    )?;
    if response
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(anyhow!(
            "running daemon did not accept native-supervisor handoff"
        ));
    }
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for daemon native-supervisor handoff"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn native_supervisor_identity(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let name = environment::SYSTEMD_UNIT_NAME;
    let artifact_path = ctx_daemon_runtime::linux_systemd_unit_path(manager_environment, name)?;
    environment::supervisor_identity(name, artifact_path).map(Some)
}

#[cfg(target_os = "macos")]
fn native_supervisor_identity(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let name = environment::LAUNCH_AGENT_LABEL;
    let artifact_path = ctx_daemon_runtime::launch_agent_path(manager_environment, name)?;
    environment::supervisor_identity(name, artifact_path).map(Some)
}

#[cfg(windows)]
fn native_supervisor_identity(
    data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    let sid = ctx_daemon_runtime::current_windows_user_sid(manager_environment)?;
    environment::windows_supervisor_identity(data_root, &sid).map(Some)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_supervisor_identity(
    _data_root: &Path,
    _manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<SupervisorIdentity>> {
    SupervisorIdentity::new(native_supervisor_kind(), PathBuf::new()).map(Some)
}

#[cfg(target_os = "linux")]
fn install_native_supervisor(
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_systemd_supervisor(
        data_root,
        spec,
        manager_environment,
        &migrate_existing_daemon_to_supervisor,
    )
}

#[cfg(target_os = "linux")]
fn disable_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    ctx_daemon_runtime::disable_systemd_supervisor(identity, manager_environment)
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_systemd_registration(spec, manager_environment)
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    let manager_pid = ctx_daemon_runtime::systemd_live_owner_pid(spec, manager_environment)?;
    ctx_daemon_runtime::verify_daemon_owner_identity(data_root, executable, Some(manager_pid))
}

#[cfg(target_os = "linux")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_systemd_supervisor(identity, manager_environment)
}

#[cfg(target_os = "macos")]
fn install_native_supervisor(
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_launch_agent(
        data_root,
        spec,
        manager_environment,
        &migrate_existing_daemon_to_supervisor,
    )
}

#[cfg(target_os = "macos")]
fn disable_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    ctx_daemon_runtime::disable_launch_agent(identity, manager_environment)
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_launch_agent_registration(spec, manager_environment)
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    let manager_pid = ctx_daemon_runtime::launch_agent_live_owner_pid(spec, manager_environment)?;
    ctx_daemon_runtime::verify_daemon_owner_identity(data_root, executable, Some(manager_pid))
}

#[cfg(target_os = "macos")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_launch_agent(identity, manager_environment)
}

#[cfg(windows)]
fn install_native_supervisor(
    data_root: &Path,
    _executable: &Path,
    _environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<PathBuf> {
    ctx_daemon_runtime::install_windows_supervisor(
        data_root,
        spec,
        manager_environment,
        &migrate_existing_daemon_to_supervisor,
    )
}

#[cfg(windows)]
fn disable_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<Option<PathBuf>> {
    ctx_daemon_runtime::disable_windows_supervisor(identity, manager_environment)
}

#[cfg(windows)]
fn verify_native_supervisor_registration(
    _data_root: &Path,
    _executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<()> {
    ctx_daemon_runtime::verify_windows_supervisor_registration(spec, manager_environment)
}

#[cfg(windows)]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    _daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
    spec: &SupervisorSpec,
) -> Result<u32> {
    ctx_daemon_runtime::verify_windows_supervisor(data_root, executable, spec, manager_environment)
}

#[cfg(windows)]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
    identity: &SupervisorIdentity,
) -> Result<()> {
    ctx_daemon_runtime::start_windows_supervisor(identity, manager_environment)
}
