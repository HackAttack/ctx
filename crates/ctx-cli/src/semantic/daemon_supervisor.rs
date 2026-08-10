use std::env;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::{fs, process::Command};

use anyhow::{anyhow, Context, Result};
#[cfg(any(test, windows))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_history_core::managed_data_root;
use serde_json::{json, Value};

use crate::compact_json;
#[cfg(unix)]
use crate::identity;

#[cfg(windows)]
use super::paths_status::daemon_root_path;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use super::{
    paths_status::{daemon_lock_is_active, pid_from_lock_json, read_pid_lock_json},
    query_service::daemon_source_refresh_request,
};

mod coordination;
mod environment;
mod report;
mod state;
#[cfg(test)]
mod tests;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(any(test, windows))]
mod windows;

use coordination::SupervisorInstallationLock;
#[cfg(target_os = "linux")]
use environment::linux_systemd_unit_with_environment;
use environment::{
    supervisor_environment_contract_report, supervisor_environment_snapshot,
    SupervisorEnvironmentSnapshot,
};
#[cfg(any(test, windows))]
use environment::{
    validated_supervisor_artifact_path, validated_supervisor_artifact_text, xml_escape,
};
pub(super) use report::daemon_supervisor_report;
#[cfg(test)]
use report::daemon_supervisor_report_with_normalized_environment;
#[cfg(any(test, target_os = "freebsd"))]
use report::freebsd_supervisor_authority_blocker;
use report::native_supervisor_product_authority_blocker;
#[cfg(test)]
use report::revalidated_supervisor_report_with;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use state::write_atomic_file;
use state::{
    native_supervisor_artifact_path, native_supervisor_kind, native_supervisor_limitation,
    stored_supervisor_report, write_installed_receipt, write_supervisor_receipt,
    write_supervisor_receipt_with_environment_snapshot, SupervisorReceipt,
};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported::*;
#[cfg(any(test, windows))]
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
    SupervisorManagerEnvironment::normalized(values)
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

#[derive(Debug, Clone, Eq, PartialEq)]
struct SupervisorManagerEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl SupervisorManagerEnvironment {
    fn normalized(values: BTreeMap<OsString, OsString>) -> Result<Self> {
        if let Some(name) = values
            .keys()
            .find(|name| release_authority_environment_name(name.as_os_str()))
        {
            return Err(anyhow!(
                "supervisor manager environment may not contain release authority variable {}",
                name.to_string_lossy()
            ));
        }
        Ok(Self { values })
    }

    fn get(&self, name: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(name)).map(OsString::as_os_str)
    }
}

trait NativeSupervisorBackend: Sync {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn install(
        &self,
        data_root: &Path,
        executable: &Path,
        environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf>;
    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()>;
    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32>;
    fn start(&self, data_root: &Path) -> Result<()>;
}

struct PlatformNativeSupervisor<'a> {
    daemon_environment: Option<&'a SupervisorEnvironmentSnapshot>,
    manager_environment: &'a SupervisorManagerEnvironment,
}

impl PlatformNativeSupervisor<'_> {
    fn launch_environment(&self) -> Result<&SupervisorEnvironmentSnapshot> {
        self.daemon_environment.ok_or_else(|| {
            anyhow!(
                "native supervisor launch or verification requires a normalized daemon environment"
            )
        })
    }
}

impl NativeSupervisorBackend for PlatformNativeSupervisor<'_> {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        native_supervisor_artifact_path(data_root, self.manager_environment)
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
        install_native_supervisor(
            data_root,
            executable,
            daemon_environment,
            self.manager_environment,
        )
    }

    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        disable_native_supervisor(data_root, self.manager_environment)
    }

    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()> {
        verify_native_supervisor_registration(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
        )
    }

    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        verify_native_supervisor(
            data_root,
            executable,
            self.launch_environment()?,
            self.manager_environment,
        )
    }

    fn start(&self, data_root: &Path) -> Result<()> {
        start_native_supervisor(data_root, self.manager_environment)
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
    let backend = PlatformNativeSupervisor {
        daemon_environment: Some(&input.daemon_environment),
        manager_environment: &input.manager_environment,
    };
    ensure_native_supervisor_with(&input, &backend)
}

fn managed_supervisor_input(data_root: &Path) -> Result<Option<ManagedSupervisorInput>> {
    safely_supported_managed_install(data_root)?
        .map(|executable| ManagedSupervisorInput::new(data_root, &executable))
        .transpose()
}

fn ensure_native_supervisor_with(
    input: &ManagedSupervisorInput,
    backend: &dyn NativeSupervisorBackend,
) -> Result<DaemonSupervisorStart> {
    let data_root = input.data_root.as_path();
    let executable = input.executable.as_path();
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    // The uninstall path disables supervisor state under this same lock. Once
    // admitted here, every artifact, manager, start, and receipt mutation below
    // remains serialized ahead of that disable.
    ensure_hosted_uninstall_supervisor_admission()?;
    let artifact = backend.artifact_path(data_root)?;

    if backend.verify_registration(data_root, executable).is_ok() {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => {
                write_installed_receipt(data_root, executable, artifact, owner_pid, None)?;
                return Ok(DaemonSupervisorStart::Native);
            }
            Err(initial_live_error) => {
                let recovery = backend
                    .start(data_root)
                    .and_then(|()| wait_for_native_live_owner(data_root, executable, backend));
                match recovery {
                    Ok(owner_pid) => {
                        write_installed_receipt(data_root, executable, artifact, owner_pid, None)?;
                        return Ok(DaemonSupervisorStart::Native);
                    }
                    Err(recovery_error) => {
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
                                    "native registration is valid but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing"
                                        .to_owned(),
                                ),
                                last_error: Some(format!(
                                    "initial live check: {initial_live_error:#}; recovery: {recovery_error:#}"
                                )),
                            },
                        )?;
                        return Ok(DaemonSupervisorStart::Fallback);
                    }
                }
            }
        }
    }

    let installation = backend
        .install(data_root, executable, &input.daemon_environment)
        .and_then(|installed_artifact| {
            wait_for_native_live_owner(data_root, executable, backend)
                .map(|owner_pid| (installed_artifact, owner_pid))
        });
    match installation {
        Ok((installed_artifact, owner_pid)) => {
            write_installed_receipt(
                data_root,
                executable,
                Some(installed_artifact),
                owner_pid,
                Some(input.daemon_environment.contract_report()),
            )?;
            Ok(DaemonSupervisorStart::Native)
        }
        Err(error) if backend.verify_registration(data_root, executable).is_ok() => {
            let recovery = backend
                .verify_live_owner(data_root, executable)
                .or_else(|_| {
                    backend.start(data_root)?;
                    wait_for_native_live_owner(data_root, executable, backend)
                });
            match recovery {
                Ok(owner_pid) => {
                    write_installed_receipt(
                        data_root,
                        executable,
                        artifact,
                        owner_pid,
                        Some(input.daemon_environment.contract_report()),
                    )?;
                    Ok(DaemonSupervisorStart::Native)
                }
                Err(recovery_error) => {
                    write_supervisor_receipt_with_environment_snapshot(
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
                                "native registration survived installation recovery but has no identity-verified live daemon owner; retrieval commands retain CLI self-healing"
                                    .to_owned(),
                            ),
                            last_error: Some(format!(
                                "installation: {error:#}; recovery: {recovery_error:#}"
                            )),
                        },
                        Some(input.daemon_environment.contract_report()),
                    )?;
                    Ok(DaemonSupervisorStart::Fallback)
                }
            }
        }
        Err(error) => {
            if let Err(cleanup_error) = backend.disable(data_root) {
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
                        artifact_path: backend.artifact_path(data_root)?,
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
    let backend = PlatformNativeSupervisor {
        daemon_environment: None,
        manager_environment: &manager_environment,
    };
    disable_native_supervisor_candidate_with(data_root, executable, &backend)
}

fn disable_native_supervisor_candidate_with(
    data_root: &Path,
    executable: Option<PathBuf>,
    backend: &dyn NativeSupervisorBackend,
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

fn wait_for_native_live_owner(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend,
) -> Result<u32> {
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    loop {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => return Ok(owner_pid),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "native supervisor did not start daemon lifecycle ownership"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
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
    let backend = PlatformNativeSupervisor {
        daemon_environment: Some(&daemon_environment),
        manager_environment: &manager_environment,
    };
    resume_daemon_supervisor_after_upgrade_with(data_root, executable, &backend, upgrade_fence)
}

fn resume_daemon_supervisor_after_upgrade_with(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend,
    upgrade_fence: &mut dyn DaemonSupervisorUpgradeFence,
) -> Result<DaemonSupervisorUpgradeResume> {
    let _installation_lock = SupervisorInstallationLock::acquire(data_root)?;
    ensure_hosted_uninstall_supervisor_admission_for_executable(executable)?;
    if backend.verify_registration(data_root, executable).is_err() {
        return Ok(DaemonSupervisorUpgradeResume::Fallback);
    }

    upgrade_fence.release()?;
    let owner = backend
        .verify_live_owner(data_root, executable)
        .or_else(|_| {
            backend.start(data_root)?;
            wait_for_native_live_owner(data_root, executable, backend)
        });
    match owner {
        Ok(owner_pid) => {
            write_installed_receipt(
                data_root,
                executable,
                backend.artifact_path(data_root)?,
                owner_pid,
                None,
            )?;
            Ok(DaemonSupervisorUpgradeResume::Native)
        }
        Err(error) => {
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
                    artifact_path: backend.artifact_path(data_root)?,
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

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn verify_daemon_owner_identity(
    data_root: &Path,
    executable: &Path,
    manager_pid: Option<u32>,
) -> Result<u32> {
    if !daemon_lock_is_active(data_root) {
        return Err(anyhow!("native supervisor has no live daemon owner lock"));
    }
    let lock = read_pid_lock_json(&super::paths_status::daemon_lock_path(data_root))
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&lock)
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no process identity"))?;
    if manager_pid.is_some_and(|expected| expected != pid) {
        return Err(anyhow!(
            "native supervisor process identity does not own the ctx daemon lock"
        ));
    }
    let recorded_executable = lock
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("native supervisor daemon lock has no executable identity"))?;
    if !same_canonical_path(recorded_executable, executable) {
        return Err(anyhow!(
            "native supervisor daemon lock does not identify the installed ctx executable"
        ));
    }
    if !super::paths_status::daemon_lock_binary_identity_matches(&lock, executable)? {
        return Err(anyhow!(
            "native supervisor daemon lock identifies a different ctx binary image"
        ));
    }
    if let Some(process_executable) = supervisor_process_executable(pid) {
        if !same_canonical_path(&process_executable, executable) {
            return Err(anyhow!(
                "native supervisor live process is not the installed ctx executable"
            ));
        }
    } else {
        return Err(anyhow!(
            "native supervisor live process executable identity is unavailable"
        ));
    }
    Ok(pid)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn same_canonical_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left).ok() == fs::canonicalize(right).ok()
}

#[cfg(target_os = "linux")]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const MAX_PATH_BYTES: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut buffer = vec![0_u8; MAX_PATH_BYTES];
    let length = unsafe {
        proc_pidpath(
            libc::pid_t::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(windows)]
fn supervisor_process_executable(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let succeeded =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &raw mut length) };
    unsafe {
        CloseHandle(handle);
    }
    (succeeded != 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).unwrap_or(0)],
        ))
    })
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
fn install_native_supervisor(
    data_root: &Path,
    executable: &Path,
    environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<PathBuf> {
    let path = linux_systemd_unit_path(manager_environment)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("systemd user unit has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create systemd user unit directory {}", parent.display()))?;
    let definition = linux_systemd_unit_with_environment(executable, data_root, environment)?;
    write_atomic_file(&path, definition.as_bytes())?;
    systemctl_user(["daemon-reload"], manager_environment)?;
    systemctl_user(["enable", "ctx.service"], manager_environment)?;
    migrate_existing_daemon_to_supervisor(data_root)?;
    start_native_supervisor(data_root, manager_environment)?;
    Ok(path)
}

#[cfg(target_os = "linux")]
fn disable_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let path = linux_systemd_unit_path(manager_environment)?;
    let disabled =
        systemctl_user_capture(["disable", "--now", "ctx.service"], manager_environment)?;
    if !disabled.status.success()
        && systemctl_user_capture(["is-enabled", "ctx.service"], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained enabled after disable: {}",
            String::from_utf8_lossy(&disabled.stderr).trim()
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove systemd user unit {}", path.display()));
        }
    }
    systemctl_user(["daemon-reload"], manager_environment)?;
    if systemctl_user_capture(["is-enabled", "ctx.service"], manager_environment)?
        .status
        .success()
        || systemctl_user_capture(["is-active", "ctx.service"], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained registered or active after removal"
        ));
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
fn linux_systemd_unit_path(manager_environment: &SupervisorManagerEnvironment) -> Result<PathBuf> {
    let root = manager_environment
        .get("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            manager_environment
                .get("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| anyhow!("resolve user configuration directory for systemd"))?;
    Ok(root.join("systemd").join("user").join("ctx.service"))
}

#[cfg(target_os = "linux")]
fn systemctl_user<const N: usize>(
    args: [&str; N],
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let output = systemctl_user_capture(args, manager_environment)?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "systemctl --user failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "linux")]
fn systemctl_user_capture<const N: usize>(
    args: [&str; N],
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<std::process::Output> {
    let mut command = supervisor_command("systemctl", manager_environment);
    command.arg("--user").args(args);
    supervisor_output(&mut command).context("run systemctl --user")
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor_registration(
    data_root: &Path,
    executable: &Path,
    daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let path = linux_systemd_unit_path(manager_environment)?;
    let registered = fs::read_to_string(&path)
        .with_context(|| format!("read systemd user unit {}", path.display()))?;
    if registered != linux_systemd_unit_with_environment(executable, data_root, daemon_environment)?
    {
        return Err(anyhow!(
            "systemd user service registration does not match the maintained definition"
        ));
    }
    let enabled = systemctl_user_capture(["is-enabled", "ctx.service"], manager_environment)?;
    if !enabled.status.success() || String::from_utf8_lossy(&enabled.stdout).trim() != "enabled" {
        return Err(anyhow!("systemd user service is not durably enabled"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    verify_native_supervisor_registration(
        data_root,
        executable,
        daemon_environment,
        manager_environment,
    )?;
    let active = systemctl_user_capture(["is-active", "ctx.service"], manager_environment)?;
    if !active.status.success() || String::from_utf8_lossy(&active.stdout).trim() != "active" {
        return Err(anyhow!("systemd user service is not active"));
    }
    let pid = systemctl_user_capture(
        ["show", "ctx.service", "--property=MainPID", "--value"],
        manager_environment,
    )?
    .stdout;
    let pid = systemd_main_pid(&pid)?;
    verify_daemon_owner_identity(data_root, executable, Some(pid))
}

#[cfg(target_os = "linux")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    systemctl_user(["start", "ctx.service"], manager_environment)
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_main_pid(output: &[u8]) -> Result<u32> {
    String::from_utf8_lossy(output)
        .trim()
        .parse::<u32>()
        .context("parse systemd user service MainPID")
        .and_then(|pid| {
            (pid != 0)
                .then_some(pid)
                .ok_or_else(|| anyhow!("systemd user service has no live MainPID"))
        })
}

#[cfg(target_os = "macos")]
fn install_native_supervisor(
    data_root: &Path,
    executable: &Path,
    environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<PathBuf> {
    let home = manager_environment
        .get("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join("rs.ctx.daemon.plist");
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("LaunchAgent has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create LaunchAgent directory {}", parent.display()))?;
    let definition =
        environment::launch_agent_plist_with_environment(executable, data_root, environment)?;
    write_atomic_file(&path, definition.as_bytes())?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(&path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    let registration = launchctl_print(&domain, manager_environment)?;
    if registration.status.success() {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    migrate_existing_daemon_to_supervisor(data_root)?;
    let mut bootstrap = supervisor_command("launchctl", manager_environment);
    bootstrap.args(["bootstrap", &domain]).arg(&path);
    command_success(&mut bootstrap, "launchctl bootstrap")?;
    start_native_supervisor(data_root, manager_environment)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn disable_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let home = manager_environment
        .get("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join("rs.ctx.daemon.plist");
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(&path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    let registration = launchctl_print(&domain, manager_environment)?;
    if registration.status.success() {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx LaunchAgent"),
    }
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
fn launchctl_print(
    domain: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<std::process::Output> {
    let mut print = supervisor_command("launchctl", manager_environment);
    print.args(["print", &format!("{domain}/rs.ctx.daemon")]);
    supervisor_output(&mut print).context("run launchctl print in GUI domain")
}

#[cfg(any(test, target_os = "macos"))]
fn launchctl_print_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key.trim() == "pid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor_registration(
    data_root: &Path,
    executable: &Path,
    daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let path = native_supervisor_artifact_path(data_root, manager_environment)?
        .ok_or_else(|| anyhow!("LaunchAgent artifact path is unavailable"))?;
    let registered = fs::read_to_string(&path)
        .with_context(|| format!("read LaunchAgent {}", path.display()))?;
    if registered
        != environment::launch_agent_plist_with_environment(
            executable,
            data_root,
            daemon_environment,
        )?
    {
        return Err(anyhow!(
            "LaunchAgent registration does not match the maintained definition"
        ));
    }
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let output = launchctl_print(&domain, manager_environment)?;
    if !output.status.success() {
        return Err(anyhow!(
            "LaunchAgent is not registered in the current GUI login domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_native_supervisor(
    data_root: &Path,
    executable: &Path,
    daemon_environment: &SupervisorEnvironmentSnapshot,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    verify_native_supervisor_registration(
        data_root,
        executable,
        daemon_environment,
        manager_environment,
    )?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let output = launchctl_print(&domain, manager_environment)?;
    let pid = launchctl_print_pid(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| anyhow!("LaunchAgent GUI registration has no live process identity"))?;
    verify_daemon_owner_identity(data_root, executable, Some(pid))
}

#[cfg(target_os = "macos")]
fn start_native_supervisor(
    _data_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let mut kickstart = supervisor_command("launchctl", manager_environment);
    kickstart.args(["kickstart", "-k", &format!("{domain}/rs.ctx.daemon")]);
    command_success(&mut kickstart, "launchctl kickstart")
}

#[cfg(any(target_os = "macos", windows))]
fn command_success(command: &mut Command, label: &str) -> Result<()> {
    let output = supervisor_output(command).with_context(|| format!("run {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn supervisor_command(
    program: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Command {
    let mut command = Command::new(program);
    command.env_clear().envs(&manager_environment.values);
    command
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn supervisor_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    command.output()
}
