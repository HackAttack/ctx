use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::{
    daemon_lock_binary_identity_matches, daemon_lock_is_active, daemon_lock_path,
    pid_from_lock_json, read_pid_lock_json,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::{write_atomic_supervisor_file, SupervisorIdentity, SupervisorSpec};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorManagerEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl SupervisorManagerEnvironment {
    pub fn new(values: BTreeMap<OsString, OsString>) -> Self {
        Self { values }
    }

    pub fn values(&self) -> &BTreeMap<OsString, OsString> {
        &self.values
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(name)).map(OsString::as_os_str)
    }
}

pub fn manager_environment_value<'a>(
    environment: &'a SupervisorManagerEnvironment,
    name: &str,
) -> Option<&'a OsStr> {
    environment.get(name)
}

pub fn supervisor_command(
    program: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Command {
    let mut command = Command::new(program);
    command.env_clear().envs(manager_environment.values());
    command
}

pub fn supervisor_output(command: &mut Command) -> std::io::Result<Output> {
    command.output()
}

pub fn command_success(command: &mut Command, label: &str) -> Result<()> {
    let output = supervisor_output(command).with_context(|| format!("run {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "linux")]
pub fn linux_systemd_unit_path(
    manager_environment: &SupervisorManagerEnvironment,
    service_name: &str,
) -> Result<PathBuf> {
    let root = manager_environment_value(manager_environment, "XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            manager_environment_value(manager_environment, "HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| anyhow!("resolve user configuration directory for systemd"))?;
    Ok(root.join("systemd").join("user").join(service_name))
}

#[cfg(target_os = "linux")]
pub fn install_systemd_supervisor(
    data_root: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
    migrate_owner: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let identity = spec.identity();
    let service_name = identity.name();
    let path = identity.artifact_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("systemd user unit has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create systemd user unit directory {}", parent.display()))?;
    write_atomic_supervisor_file(path, super::linux_systemd_unit(spec)?.as_bytes())?;
    systemctl_user(["daemon-reload"], manager_environment)?;
    systemctl_user(["enable", service_name], manager_environment)?;
    migrate_owner(data_root)?;
    start_systemd_supervisor(identity, manager_environment)?;
    Ok(path.to_path_buf())
}

#[cfg(target_os = "linux")]
pub fn disable_systemd_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let service_name = identity.name();
    let path = identity.artifact_path();
    let disabled = systemctl_user_capture(["disable", "--now", service_name], manager_environment)?;
    if !disabled.status.success()
        && systemctl_user_capture(["is-enabled", service_name], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained enabled after disable: {}",
            String::from_utf8_lossy(&disabled.stderr).trim()
        ));
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove systemd user unit {}", path.display()))
        }
    }
    systemctl_user(["daemon-reload"], manager_environment)?;
    if systemctl_user_capture(["is-enabled", service_name], manager_environment)?
        .status
        .success()
        || systemctl_user_capture(["is-active", service_name], manager_environment)?
            .status
            .success()
    {
        return Err(anyhow!(
            "systemd user service remained registered or active after removal"
        ));
    }
    Ok(Some(path.to_path_buf()))
}

#[cfg(target_os = "linux")]
pub fn verify_systemd_registration(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let identity = spec.identity();
    let service_name = identity.name();
    let path = identity.artifact_path();
    let registered = fs::read_to_string(path)
        .with_context(|| format!("read systemd user unit {}", path.display()))?;
    if registered != super::linux_systemd_unit(spec)? {
        return Err(anyhow!(
            "systemd user service registration does not match the maintained definition"
        ));
    }
    let enabled = systemctl_user_capture(["is-enabled", service_name], manager_environment)?;
    if !enabled.status.success() || String::from_utf8_lossy(&enabled.stdout).trim() != "enabled" {
        return Err(anyhow!("systemd user service is not durably enabled"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn systemd_live_owner_pid(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    let service_name = spec.identity().name();
    verify_systemd_registration(spec, manager_environment)?;
    let active = systemctl_user_capture(["is-active", service_name], manager_environment)?;
    if !active.status.success() || String::from_utf8_lossy(&active.stdout).trim() != "active" {
        return Err(anyhow!("systemd user service is not active"));
    }
    let output = systemctl_user_capture(
        ["show", service_name, "--property=MainPID", "--value"],
        manager_environment,
    )?;
    systemd_main_pid(&output.stdout)
}

#[cfg(target_os = "linux")]
pub fn start_systemd_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    systemctl_user(["start", identity.name()], manager_environment)
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
pub fn systemctl_user_capture<const N: usize>(
    args: [&str; N],
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Output> {
    let mut command = supervisor_command("systemctl", manager_environment);
    command.arg("--user").args(args);
    supervisor_output(&mut command).context("run systemctl --user")
}

pub fn systemd_main_pid(output: &[u8]) -> Result<u32> {
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
pub fn launch_agent_path(
    manager_environment: &SupervisorManagerEnvironment,
    label: &str,
) -> Result<PathBuf> {
    let home = manager_environment_value(manager_environment, "HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("resolve user home for LaunchAgent"))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn launchctl_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
pub fn install_launch_agent(
    data_root: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
    migrate_owner: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let identity = spec.identity();
    let path = identity.artifact_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("LaunchAgent has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create LaunchAgent directory {}", parent.display()))?;
    write_atomic_supervisor_file(path, super::launch_agent_plist(spec)?.as_bytes())?;
    let domain = launchctl_domain();
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(&path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    if launchctl_print(&domain, identity.name(), manager_environment)?
        .status
        .success()
    {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    migrate_owner(data_root)?;
    let mut bootstrap = supervisor_command("launchctl", manager_environment);
    bootstrap.args(["bootstrap", &domain]).arg(&path);
    command_success(&mut bootstrap, "launchctl bootstrap")?;
    start_launch_agent(identity, manager_environment)?;
    Ok(path.to_path_buf())
}

#[cfg(target_os = "macos")]
pub fn disable_launch_agent(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let label = identity.name();
    let path = identity.artifact_path();
    let domain = launchctl_domain();
    let mut bootout = supervisor_command("launchctl", manager_environment);
    bootout.args(["bootout", &domain]).arg(&path);
    let bootout = supervisor_output(&mut bootout).context("run launchctl bootout")?;
    if launchctl_print(&domain, label, manager_environment)?
        .status
        .success()
    {
        return Err(anyhow!(
            "LaunchAgent remained registered after bootout: {}",
            String::from_utf8_lossy(&bootout.stderr).trim()
        ));
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx LaunchAgent"),
    }
    Ok(Some(path.to_path_buf()))
}

#[cfg(target_os = "macos")]
pub fn verify_launch_agent_registration(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let identity = spec.identity();
    let path = identity.artifact_path();
    let registered =
        fs::read_to_string(path).with_context(|| format!("read LaunchAgent {}", path.display()))?;
    if registered != super::launch_agent_plist(spec)? {
        return Err(anyhow!(
            "LaunchAgent registration does not match the maintained definition"
        ));
    }
    let output = launchctl_print(&launchctl_domain(), identity.name(), manager_environment)?;
    if !output.status.success() {
        return Err(anyhow!(
            "LaunchAgent is not registered in the current GUI login domain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn launch_agent_live_owner_pid(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    verify_launch_agent_registration(spec, manager_environment)?;
    let output = launchctl_print(
        &launchctl_domain(),
        spec.identity().name(),
        manager_environment,
    )?;
    launchctl_print_pid(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| anyhow!("LaunchAgent GUI registration has no live process identity"))
}

#[cfg(target_os = "macos")]
fn launchctl_print(
    domain: &str,
    label: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Output> {
    let mut print = supervisor_command("launchctl", manager_environment);
    print.args(["print", &format!("{domain}/{label}")]);
    supervisor_output(&mut print).context("run launchctl print in GUI domain")
}

pub fn launchctl_print_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        (key.trim() == "pid")
            .then(|| value.trim().parse::<u32>().ok())
            .flatten()
    })
}

#[cfg(target_os = "macos")]
pub fn start_launch_agent(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let domain = launchctl_domain();
    let mut kickstart = supervisor_command("launchctl", manager_environment);
    kickstart.args(["kickstart", "-k", &format!("{domain}/{}", identity.name())]);
    command_success(&mut kickstart, "launchctl kickstart")
}

pub fn verify_daemon_owner_identity(
    data_root: &Path,
    executable: &Path,
    manager_pid: Option<u32>,
) -> Result<u32> {
    if !daemon_lock_is_active(data_root) {
        return Err(anyhow!("native supervisor has no live daemon owner lock"));
    }
    let lock = read_pid_lock_json(&daemon_lock_path(data_root))
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
    if !daemon_lock_binary_identity_matches(&lock, executable)? {
        return Err(anyhow!(
            "native supervisor daemon lock identifies a different ctx binary image"
        ));
    }
    let process_executable = supervisor_process_executable(pid).ok_or_else(|| {
        anyhow!("native supervisor live process executable identity is unavailable")
    })?;
    if !same_canonical_path(&process_executable, executable) {
        return Err(anyhow!(
            "native supervisor live process is not the installed ctx executable"
        ));
    }
    Ok(pid)
}

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
    unsafe { CloseHandle(handle) };
    (succeeded != 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &buffer[..usize::try_from(length).unwrap_or(0)],
        ))
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn supervisor_process_executable(_pid: u32) -> Option<PathBuf> {
    None
}
