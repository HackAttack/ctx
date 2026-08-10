use super::*;

mod termination;
use termination::terminate_identity_verified_legacy_daemon;
pub(super) use termination::{
    terminate_identity_verified_residual_daemon, terminate_identity_verified_residual_daemon_owner,
};

struct CurrentHandoffSupervisorFence<'a> {
    handoff: &'a mut DaemonUpgradeHandoff,
}

impl super::super::daemon_supervisor::DaemonSupervisorUpgradeFence
    for CurrentHandoffSupervisorFence<'_>
{
    fn release(&mut self) -> Result<()> {
        self.handoff.complete_release()
    }
}

struct ReplacementHandoffSupervisorFence<'a> {
    data_root: &'a Path,
    handoff_id: &'a str,
}

impl super::super::daemon_supervisor::DaemonSupervisorUpgradeFence
    for ReplacementHandoffSupervisorFence<'_>
{
    fn release(&mut self) -> Result<()> {
        write_daemon_upgrade_handoff(self.data_root, self.handoff_id, "completed", None)
    }
}

pub(in crate::semantic) fn terminate_current_executable_daemon(data_root: &Path) -> Result<()> {
    let executable = env::current_exe().context("resolve current ctx executable")?;
    terminate_identity_verified_residual_daemon(data_root, &executable)
}

fn daemon_upgrade_handoff_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_HANDOFF_FILE)
}

fn daemon_upgrade_restart_request_root(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_UPGRADE_RESTART_REQUEST_DIR)
}

const DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV: &str =
    "CTX_DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_FOR_TESTS";

pub(super) fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

pub(super) fn read_daemon_upgrade_handoff(data_root: &Path) -> Option<Value> {
    read_daemon_upgrade_handoff_at(&daemon_upgrade_handoff_path(data_root))
}

fn read_daemon_upgrade_handoff_at(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DaemonUpgradeHandoffState {
    Absent,
    Terminal,
    Active,
    CorruptOrUnreadable,
}

fn daemon_upgrade_handoff_state_at(path: &Path) -> DaemonUpgradeHandoffState {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DaemonUpgradeHandoffState::Absent
        }
        Err(_) => return DaemonUpgradeHandoffState::CorruptOrUnreadable,
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return DaemonUpgradeHandoffState::CorruptOrUnreadable;
    };
    let Some(handoff_id) = value.get("handoff_id").and_then(Value::as_str) else {
        return DaemonUpgradeHandoffState::CorruptOrUnreadable;
    };
    if handoff_id.is_empty() {
        return DaemonUpgradeHandoffState::CorruptOrUnreadable;
    }
    let Some(phase) = value.get("phase").and_then(Value::as_str) else {
        return DaemonUpgradeHandoffState::CorruptOrUnreadable;
    };
    if matches!(phase, "completed" | "aborted") {
        return DaemonUpgradeHandoffState::Terminal;
    }
    let pid_key = match phase {
        "scheduled" => "helper_pid",
        "preparing" | "ready" => "owner_pid",
        _ => return DaemonUpgradeHandoffState::CorruptOrUnreadable,
    };
    let Some(pid) = value
        .get(pid_key)
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
    else {
        return DaemonUpgradeHandoffState::CorruptOrUnreadable;
    };
    let marker_is_fresh = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .is_some_and(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age <= DAEMON_UPGRADE_HANDOFF_STALE_AFTER)
        });
    if !marker_is_fresh {
        return DaemonUpgradeHandoffState::Terminal;
    }
    match process_state(pid) {
        ProcessState::Running | ProcessState::Unknown => DaemonUpgradeHandoffState::Active,
        ProcessState::NotRunning => DaemonUpgradeHandoffState::Terminal,
    }
}

#[cfg(test)]
pub(super) fn daemon_upgrade_handoff_is_active(data_root: &Path) -> bool {
    let path = daemon_upgrade_handoff_path(data_root);
    daemon_upgrade_handoff_is_active_at(&path)
}

fn daemon_upgrade_handoff_is_active_at(path: &Path) -> bool {
    daemon_upgrade_handoff_state_at(path) == DaemonUpgradeHandoffState::Active
}

pub(in crate::semantic) fn daemon_upgrade_handoff_blocks_current_process(data_root: &Path) -> bool {
    match daemon_upgrade_handoff_state_at(&daemon_upgrade_handoff_path(data_root)) {
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => false,
        DaemonUpgradeHandoffState::CorruptOrUnreadable => true,
        DaemonUpgradeHandoffState::Active => !current_process_owns_daemon_upgrade_handoff(data_root),
    }
}

pub(super) fn daemon_upgrade_handoff_fences_start(data_root: &Path) -> bool {
    !matches!(
        daemon_upgrade_handoff_state_at(&daemon_upgrade_handoff_path(data_root)),
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal
    )
}

pub(in crate::semantic) fn current_process_owns_daemon_upgrade_handoff(data_root: &Path) -> bool {
    let token = env::var(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV).ok();
    current_process_owns_daemon_upgrade_handoff_with_token(data_root, token.as_deref())
}

fn current_process_owns_daemon_upgrade_handoff_with_token(
    data_root: &Path,
    handoff_token: Option<&str>,
) -> bool {
    current_process_owns_daemon_upgrade_handoff_at(
        &daemon_upgrade_handoff_path(data_root),
        handoff_token,
    )
}

fn current_process_owns_daemon_upgrade_handoff_at(
    handoff_path: &Path,
    handoff_token: Option<&str>,
) -> bool {
    if !daemon_upgrade_handoff_is_active_at(handoff_path) {
        return false;
    }
    let expected = read_daemon_upgrade_handoff_at(handoff_path).and_then(|value| {
        value
            .get("handoff_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    expected.as_deref().is_some() && expected.as_deref() == handoff_token
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ExpectedProcessIdentity {
    executable: PathBuf,
}

trait CooperativeStopPort: Send {
    fn request_stop(&mut self);
}

struct SourceRefreshCooperativeStopPort {
    data_root: PathBuf,
}

impl CooperativeStopPort for SourceRefreshCooperativeStopPort {
    fn request_stop(&mut self) {
        let _ = daemon_source_refresh_request(
            &self.data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": "lifecycle_wakeup",
            })),
            DAEMON_HEALTH_TIMEOUT,
            DAEMON_HEALTH_RESPONSE_MAX_BYTES,
        );
    }
}

struct DaemonUpgradeHandoffInput {
    data_root: PathBuf,
    handoff_id: String,
    expected_process: ExpectedProcessIdentity,
    persisted_restart_label: Option<String>,
    allow_cooperative_grace: bool,
    handoff_path: PathBuf,
    restart_request_root: PathBuf,
    cooperative_stop: Box<dyn CooperativeStopPort>,
}

fn normalize_daemon_upgrade_handoff_input(
    data_root: &Path,
    upgrade_attempt_id: &str,
    expected_executable: &Path,
    allow_cooperative_grace: bool,
) -> Result<DaemonUpgradeHandoffInput> {
    if !crate::upgrade::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    Ok(DaemonUpgradeHandoffInput {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        expected_process: ExpectedProcessIdentity {
            executable: expected_executable.to_path_buf(),
        },
        persisted_restart_label: daemon_restart_trigger(data_root)
            .map(|trigger| trigger.as_str().to_owned()),
        allow_cooperative_grace,
        handoff_path: daemon_upgrade_handoff_path(data_root),
        restart_request_root: daemon_upgrade_restart_request_root(data_root),
        cooperative_stop: Box::new(SourceRefreshCooperativeStopPort {
            data_root: data_root.to_path_buf(),
        }),
    })
}

pub(crate) struct DaemonUpgradeHandoff {
    data_root: PathBuf,
    handoff_id: String,
    installation_executable: PathBuf,
    persisted_restart_label: Option<String>,
    release_on_drop: bool,
}

struct UpgradeHandoffRestartAuthority {
    replacement_executable: PathBuf,
}

impl UpgradeHandoffRestartAuthority {
    fn spawn(&self, launch: DetachedDaemonLaunch) -> io::Result<Child> {
        spawn_daemon_child_for_upgrade_handoff(launch, &self.replacement_executable)
    }
}

impl DaemonUpgradeHandoff {
    pub(crate) fn wait_for_installation_quiescence(&self) -> Result<()> {
        wait_for_installation_daemon_quiescence_for(
            &self.installation_executable,
            &self.handoff_id,
        )?;
        pause_after_installation_quiescence_for_test()
    }

    /// Capture the effective auto-daemon restart request in data that can be
    /// embedded in a durable platform replacement helper.
    pub(crate) fn replacement_restart(&self) -> Option<(&'static str, u64, u64)> {
        let trigger = self
            .persisted_restart_label
            .as_deref()
            .and_then(|label| parse_daemon_trigger(Some(label)))
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger))?;
        Some((
            trigger.as_str(),
            daemon_autostart_u64_env(
                "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
                DAEMON_IDLE_EXIT_SECONDS_CAP,
            )
            .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_CAP),
            daemon_autostart_u64_env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", 3_600)
                .unwrap_or(15 * 60),
        ))
    }

    /// Preserve daemon restart intent while schema-2 recovery re-executes the
    /// identity-validated current-format executable restored at the install
    /// path. The restored process consumes this request while fixing forward.
    pub(crate) fn release_for_current_format_reexec(mut self) -> Result<()> {
        if read_daemon_restart_request(&self.data_root).is_none() {
            if let Some(label) = self.persisted_restart_label.as_deref() {
                write_daemon_restart_request_at(
                    &daemon_upgrade_restart_request_root(&self.data_root),
                    label,
                    &self.handoff_id,
                )?;
            }
        }
        self.release("aborted", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    /// Release the upgrade fence and restart the current auto-daemon after a
    /// verified forward publication succeeds.
    pub(crate) fn resume_with(mut self, executable: &Path) -> Result<()> {
        let restart_authority = self.authenticated_restart_authority(executable)?;
        let restart_trigger = self
            .persisted_restart_label
            .as_deref()
            .and_then(|label| parse_daemon_trigger(Some(label)))
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger));
        if daemon_restart_allowed(&self.data_root)? {
            if let Some(trigger) = restart_trigger {
                let data_root = self.data_root.clone();
                let mut upgrade_fence = CurrentHandoffSupervisorFence { handoff: &mut self };
                let supervisor_resume =
                    super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                        &data_root,
                        executable,
                        &mut upgrade_fence,
                    )?;
                match supervisor_resume {
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                        wait_for_daemon_ready_ack(&self.data_root)?;
                    }
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback => {
                        let launch = configured_daemon_autostart_command(
                            executable,
                            &self.data_root,
                            trigger,
                            Some(&self.handoff_id),
                        )?;
                        let mut child = restart_authority
                            .spawn(launch)
                            .context("restart ctx daemon after upgrade")?;
                        wait_for_replacement_daemon(&self.data_root, &mut child)?;
                    }
                }
            }
        }
        remove_daemon_restart_requests(&self.data_root);
        restart_acknowledged_installation_daemons_with(
            executable,
            &self.handoff_id,
            Some(&self.data_root),
            |launch| restart_authority.spawn(launch),
        )?;
        if self.release_on_drop {
            self.complete_release()?;
        }
        Ok(())
    }

    fn authenticated_restart_authority(
        &self,
        executable: &Path,
    ) -> Result<UpgradeHandoffRestartAuthority> {
        let current = read_daemon_upgrade_handoff(&self.data_root)
            .ok_or_else(|| anyhow!("daemon upgrade handoff disappeared before restart"))?;
        let identity_matches = current.get("handoff_id").and_then(Value::as_str)
            == Some(self.handoff_id.as_str())
            && current.get("phase").and_then(Value::as_str) == Some("ready")
            && current
                .get("owner_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(process::id());
        if !identity_matches {
            return Err(anyhow!(
                "current process does not own the ready daemon upgrade handoff"
            ));
        }
        Ok(UpgradeHandoffRestartAuthority {
            replacement_executable: executable.to_path_buf(),
        })
    }

    /// Keep the fence owned by a platform replacement helper after apply
    /// returns `Scheduled`. Autostart remains blocked while that helper is live
    /// and becomes eligible only after it exits.
    pub(crate) fn transfer_to_replacement_helper(mut self, helper_pid: u32) -> Result<()> {
        let already_transferred =
            read_daemon_upgrade_handoff(&self.data_root).is_some_and(|value| {
                value.get("handoff_id").and_then(Value::as_str) == Some(self.handoff_id.as_str())
                    && value.get("phase").and_then(Value::as_str) == Some("scheduled")
                    && value
                        .get("helper_pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok())
                        == Some(helper_pid)
            });
        if !already_transferred {
            self.release("scheduled", Some(helper_pid))?;
        }
        self.release_on_drop = false;
        Ok(())
    }

    fn release(&self, phase: &str, helper_pid: Option<u32>) -> Result<()> {
        let current = read_daemon_upgrade_handoff(&self.data_root);
        if current
            .as_ref()
            .and_then(|value| value.get("handoff_id").and_then(Value::as_str))
            != Some(self.handoff_id.as_str())
        {
            return Ok(());
        }
        write_daemon_upgrade_handoff(&self.data_root, &self.handoff_id, phase, helper_pid)
    }

    fn complete_release(&mut self) -> Result<()> {
        self.release("completed", None)?;
        self.release_on_drop = false;
        Ok(())
    }
}

impl Drop for DaemonUpgradeHandoff {
    fn drop(&mut self) {
        if self.release_on_drop {
            let _ = self.release("aborted", None);
        }
    }
}

/// Fence daemon starts, request a cooperative exit from the current daemon, and
/// wait until its process lock is released before binary replacement begins.
///
/// The actual upgrade owner must already hold the upgrade transaction lock.
/// This handoff deliberately does not schedule or serialize upgrades.
pub(crate) fn begin_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
) -> Result<DaemonUpgradeHandoff> {
    let expected_executable = env::current_exe().context("resolve upgrading ctx executable")?;
    let input = normalize_daemon_upgrade_handoff_input(
        data_root,
        upgrade_attempt_id,
        &expected_executable,
        true,
    )?;
    begin_daemon_upgrade_handoff_with(input)
}

pub(crate) fn begin_legacy_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
    expected_executable: &Path,
) -> Result<DaemonUpgradeHandoff> {
    let input = normalize_daemon_upgrade_handoff_input(
        data_root,
        upgrade_attempt_id,
        expected_executable,
        false,
    )?;
    begin_daemon_upgrade_handoff_with(input)
}

fn begin_daemon_upgrade_handoff_with(
    input: DaemonUpgradeHandoffInput,
) -> Result<DaemonUpgradeHandoff> {
    let DaemonUpgradeHandoffInput {
        data_root,
        handoff_id,
        expected_process,
        persisted_restart_label,
        allow_cooperative_grace,
        handoff_path,
        restart_request_root,
        mut cooperative_stop,
    } = input;
    match daemon_upgrade_handoff_state_at(&handoff_path) {
        DaemonUpgradeHandoffState::Active => {
            return Err(anyhow!("another ctx upgrade owns the daemon lifecycle handoff"));
        }
        DaemonUpgradeHandoffState::CorruptOrUnreadable => {
            return Err(anyhow!("daemon upgrade handoff state is corrupt or unreadable"));
        }
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => {}
    }
    persist_handoff_before_cooperative_stop(
        &handoff_path,
        &restart_request_root,
        &handoff_id,
        persisted_restart_label.as_deref(),
        cooperative_stop.as_mut(),
    )?;
    let handoff = DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        handoff_id,
        installation_executable: expected_process.executable.clone(),
        persisted_restart_label,
        release_on_drop: true,
    };
    if !allow_cooperative_grace && daemon_lock_is_active(&data_root) {
        terminate_identity_verified_legacy_daemon(&data_root, &expected_process.executable)
            .context("stop identity-verified legacy ctx daemon before automatic upgrade")?;
    }
    let deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while daemon_lock_is_active(&data_root) {
        if Instant::now() >= deadline {
            #[cfg(any(unix, windows))]
            {
                if allow_cooperative_grace {
                    terminate_identity_verified_residual_daemon(
                        &data_root,
                        &expected_process.executable,
                    )
                    .context("stop residual ctx daemon before upgrade")?;
                } else {
                    terminate_identity_verified_legacy_daemon(
                        &data_root,
                        &expected_process.executable,
                    )
                    .context("stop residual legacy ctx daemon before automatic upgrade")?;
                }
                break;
            }
            #[cfg(not(any(unix, windows)))]
            return Err(anyhow!(
                "timed out waiting for the ctx daemon to stop before upgrade"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    wait_for_daemon_lifecycle_release(&data_root)?;
    write_daemon_upgrade_handoff_at(&handoff_path, &handoff.handoff_id, "ready", None)?;
    handoff.wait_for_installation_quiescence()?;
    Ok(handoff)
}

fn persist_handoff_before_cooperative_stop(
    handoff_path: &Path,
    restart_request_root: &Path,
    handoff_id: &str,
    persisted_restart_label: Option<&str>,
    cooperative_stop: &mut dyn CooperativeStopPort,
) -> Result<()> {
    // A process crash after this point must leave restart intent and the
    // lifecycle fence durable before the stop request can be observed.
    if let Some(label) = persisted_restart_label {
        write_daemon_restart_request_at(restart_request_root, label, handoff_id)?;
    }
    write_daemon_upgrade_handoff_at(handoff_path, handoff_id, "preparing", None)?;
    cooperative_stop.request_stop();
    Ok(())
}

/// Hosted uninstallers call this command before deleting the installed
/// executable. Each phase is idempotent so an interrupted uninstaller can
/// invoke it again safely.
pub(crate) fn prepare_daemon_uninstall(data_root: &Path) -> Result<Value> {
    let expected_executable =
        env::current_exe().context("resolve installed ctx executable before uninstall")?;
    let canonical_root =
        ctx_history_core::managed_data_root().context("resolve canonical ctx data root")?;
    let mut roots = BTreeSet::from([data_root.to_path_buf(), canonical_root.clone()]);
    let mut disabled_roots = BTreeSet::new();
    discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
    if cfg!(debug_assertions) && env::var_os(DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV).is_some() {
        process::exit(89);
    }

    super::super::daemon_supervisor::disable_daemon_supervisor(&canonical_root)
        .context("remove canonical ctx daemon supervisor before uninstall")?;

    let installation_deadline = Instant::now() + DAEMON_INSTALLATION_QUIESCE_TIMEOUT;
    let installation_quiescence = loop {
        discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
        quiesce_daemon_roots(&roots, &expected_executable)?;
        if let Some(quiescence) = super::installation::try_acquire_installation_daemon_quiescence()?
        {
            break quiescence;
        }
        if Instant::now() >= installation_deadline {
            return Err(anyhow!(
                "timed out waiting for installation-wide ctx daemon quiescence; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    };

    discover_and_disable_installation_roots(&mut roots, &mut disabled_roots)?;
    for root in &roots {
        if daemon_lock_is_active(root) {
            return Err(anyhow!(
                "ctx daemon lifecycle ownership appeared after installation quiescence for {}; keep the ctx binary and retry `ctx daemon disable --prepare-uninstall`",
                root.display()
            ));
        }
        super::super::cancel_core_finalization_generation_lease(
            root,
            "daemon was disabled for uninstall",
        )?;
    }
    super::installation::remove_installation_daemon_coordination()
        .context("remove installation-wide ctx daemon coordination before uninstall")?;
    for root in &roots {
        remove_daemon_lifecycle_coordination(root)?;
    }
    drop(installation_quiescence);
    let quiesced_roots = roots.into_iter().collect::<Vec<_>>();
    let quiesced_root_count = quiesced_roots.len();
    Ok(compact_json(json!({
        "schema_version": 1,
        "command": "daemon_prepare_uninstall",
        "ok": true,
        "scope": "installation",
        "requested_data_root": data_root,
        "canonical_data_root": canonical_root,
        "quiesced_roots": quiesced_roots,
        "quiesced_root_count": quiesced_root_count,
        "installation_quiescent": true,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": true,
        "endpoint_released": true,
        "supervisor_removed": true,
        "coordination_state_removed": true,
        "binary_retained": true,
        "retry_safe": true,
        "local_only": true,
    })))
}

fn discover_and_disable_installation_roots(
    roots: &mut BTreeSet<PathBuf>,
    disabled_roots: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    roots.extend(super::installation::registered_installation_daemon_roots()?);
    for root in roots.iter() {
        if disabled_roots.insert(root.clone()) {
            crate::config::set_daemon_enabled(root, false).with_context(|| {
                format!(
                    "durably disable ctx daemon root {} before uninstall",
                    root.display()
                )
            })?;
        }
    }
    Ok(())
}

fn quiesce_daemon_roots(roots: &BTreeSet<PathBuf>, expected_executable: &Path) -> Result<()> {
    for root in roots {
        request_disabled_daemon_shutdown(root);
    }
    let cooperative_deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while roots.iter().any(|root| daemon_lock_is_active(root))
        && Instant::now() < cooperative_deadline
    {
        for root in roots {
            if daemon_lock_is_active(root) {
                request_disabled_daemon_shutdown(root);
            }
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    for root in roots {
        if daemon_lock_is_active(root) {
            terminate_identity_verified_residual_daemon(root, expected_executable).with_context(
                || {
                    format!(
                        "stop identity-verified residual ctx daemon for {} before uninstall",
                        root.display()
                    )
                },
            )?;
        }
        wait_for_daemon_lifecycle_release(root)?;
    }
    Ok(())
}

fn request_disabled_daemon_shutdown(data_root: &Path) {
    let _ = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "shutdown",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    );
}

fn wait_for_daemon_lifecycle_release(data_root: &Path) -> Result<()> {
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "ctx daemon retained lifecycle ownership after verified termination"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    Ok(())
}

fn remove_daemon_lifecycle_coordination(data_root: &Path) -> Result<()> {
    remove_daemon_restart_requests(data_root);
    let root = daemon_root_path(data_root);
    for path in [
        daemon_upgrade_handoff_path(data_root),
        daemon_query_endpoint_path(data_root),
        root.join("source-refresh-endpoint.json"),
        root.join("query.sock"),
        root.join("source-refresh.sock"),
        root.join("supervisor.json"),
        daemon_lock_path(data_root),
        pid_lock_guard_path(&daemon_lock_path(data_root)),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove daemon coordination {}", path.display()))
            }
        }
    }
    Ok(())
}

/// Fence new daemon starts while the daemon that owns `data_root` is still
/// quiescing. Unlike the manual path, this must not wait for the daemon lock:
/// the caller is that daemon and will release the lock only after this fence is
/// durable.
pub(crate) fn begin_current_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
    restart_trigger: DaemonTriggerCommandArg,
) -> Result<DaemonUpgradeHandoff> {
    if !crate::upgrade::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    let input = CurrentDaemonUpgradeHandoffInput {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        persisted_restart_label: restart_trigger.as_str().to_owned(),
        installation_executable: env::current_exe().context("resolve upgrading ctx executable")?,
        current_handoff_token: env::var(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV).ok(),
        handoff_path: daemon_upgrade_handoff_path(data_root),
        restart_request_root: daemon_upgrade_restart_request_root(data_root),
    };
    begin_current_daemon_upgrade_handoff_with(input)
}

#[derive(Debug)]
struct CurrentDaemonUpgradeHandoffInput {
    data_root: PathBuf,
    handoff_id: String,
    persisted_restart_label: String,
    installation_executable: PathBuf,
    current_handoff_token: Option<String>,
    handoff_path: PathBuf,
    restart_request_root: PathBuf,
}

fn begin_current_daemon_upgrade_handoff_with(
    input: CurrentDaemonUpgradeHandoffInput,
) -> Result<DaemonUpgradeHandoff> {
    let CurrentDaemonUpgradeHandoffInput {
        data_root,
        handoff_id,
        persisted_restart_label,
        installation_executable,
        current_handoff_token,
        handoff_path,
        restart_request_root,
    } = input;
    if !daemon_lock_is_active(&data_root) {
        return Err(anyhow!(
            "automatic upgrade handoff requires current daemon ownership"
        ));
    }
    match daemon_upgrade_handoff_state_at(&handoff_path) {
        DaemonUpgradeHandoffState::CorruptOrUnreadable => {
            return Err(anyhow!("daemon upgrade handoff state is corrupt or unreadable"));
        }
        DaemonUpgradeHandoffState::Active => {
        let current = read_daemon_upgrade_handoff_at(&handoff_path)
            .ok_or_else(|| anyhow!("active daemon handoff disappeared"))?;
        if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id.as_str())
            || !current_process_owns_daemon_upgrade_handoff_at(
                &handoff_path,
                current_handoff_token.as_deref(),
            )
        {
            return Err(anyhow!(
                "another ctx upgrade owns the daemon lifecycle handoff"
            ));
        }
        return Ok(DaemonUpgradeHandoff {
            data_root,
            handoff_id,
            installation_executable,
            persisted_restart_label: Some(persisted_restart_label),
            release_on_drop: true,
        });
        }
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => {}
    }
    write_daemon_restart_request_at(&restart_request_root, &persisted_restart_label, &handoff_id)?;
    write_daemon_upgrade_handoff_at(&handoff_path, &handoff_id, "ready", None)?;
    Ok(DaemonUpgradeHandoff {
        data_root,
        handoff_id,
        installation_executable,
        persisted_restart_label: Some(persisted_restart_label),
        release_on_drop: true,
    })
}

fn pause_after_installation_quiescence_for_test() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Some(path) = env::var_os("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, b"ready\n")?;
    let release = path.with_extension("continue");
    let deadline = Instant::now() + StdDuration::from_secs(15);
    while !release.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting to continue after test installation quiescence"
            ));
        }
        std::thread::sleep(StdDuration::from_millis(25));
    }
    Ok(())
}

/// Make helper ownership durable before its parent accepts the readiness
/// receipt. This closes the parent-exit window in which a live replacement
/// helper could otherwise lose the daemon-start fence.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn mark_replacement_helper_handoff(
    data_root: &Path,
    handoff_id: &str,
    helper_pid: u32,
) -> Result<()> {
    if helper_pid == 0 {
        return Err(anyhow!("replacement helper PID must be nonzero"));
    }
    let current = read_daemon_upgrade_handoff(data_root)
        .ok_or_else(|| anyhow!("replacement helper has no daemon handoff"))?;
    if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
        return Err(anyhow!(
            "replacement helper daemon handoff identity does not match"
        ));
    }
    write_daemon_upgrade_handoff(data_root, handoff_id, "scheduled", Some(helper_pid))
}

/// Complete a durable replacement handoff from the Windows helper.
///
/// The helper passes the origin-root identity and daemon parameters captured
/// before the old daemon stopped. Success means either no daemon had been
/// running, or the replacement process owns the existing daemon lifecycle
/// lock; a successful `spawn` alone is never treated as readiness.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn complete_replacement_daemon_handoff(
    data_root: &Path,
    executable: &Path,
    handoff_id: &str,
    restart: Option<(&str, u64, u64)>,
) -> Result<()> {
    if let Some(current) = read_daemon_upgrade_handoff(data_root) {
        if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
            return Err(anyhow!(
                "replacement daemon handoff identity does not match its install journal"
            ));
        }
    }
    let captured_restart = if let Some((trigger, idle_exit, loop_interval)) = restart {
        Some((
            parse_daemon_trigger(Some(trigger))
                .ok_or_else(|| anyhow!("replacement daemon handoff has an invalid trigger"))?,
            idle_exit,
            loop_interval,
        ))
    } else {
        None
    };
    let requested_trigger = read_daemon_restart_request(data_root).map(|(_path, trigger)| trigger);
    if let Some(trigger) = captured_restart
        .map(|(trigger, _, _)| trigger)
        .or(requested_trigger)
    {
        if !daemon_lock_is_active(data_root) {
            // Recreate the durable acknowledgement token if an earlier ready
            // daemon consumed it and then exited before handoff completion.
            if read_daemon_restart_request(data_root).is_none() {
                write_daemon_restart_request(data_root, trigger, handoff_id)?;
            }
            let mut upgrade_fence = ReplacementHandoffSupervisorFence {
                data_root,
                handoff_id,
            };
            let supervisor_resume =
                super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                    data_root,
                    executable,
                    &mut upgrade_fence,
                )?;
            match supervisor_resume {
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                    wait_for_daemon_ready_ack(data_root)?;
                }
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback => {
                    let launch =
                        if let Some((_trigger, idle_exit, loop_interval)) = captured_restart {
                            daemon_autostart_command(
                                executable,
                                data_root,
                                trigger,
                                (idle_exit != DAEMON_IDLE_EXIT_SECONDS_CAP).then_some(idle_exit),
                                Some(loop_interval),
                                Some(handoff_id),
                            )
                        } else {
                            configured_daemon_autostart_command(
                                executable,
                                data_root,
                                trigger,
                                Some(handoff_id),
                            )
                        }?;
                    let mut child = spawn_daemon_child(launch)
                        .context("restart ctx daemon after replacement")?;
                    wait_for_replacement_daemon(data_root, &mut child)?;
                }
            }
        } else {
            wait_for_daemon_ready_ack(data_root)?;
        }
        if !daemon_lock_is_active(data_root) || read_daemon_restart_request(data_root).is_some() {
            return Err(anyhow!(
                "replacement ctx daemon did not reach lifecycle readiness"
            ));
        }
    }
    Ok(())
}

/// Mark the helper-owned handoff complete only after its terminal journal is
/// durable and its installation lock has been released.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn finish_replacement_daemon_handoff(data_root: &Path, handoff_id: &str) -> Result<()> {
    if read_daemon_upgrade_handoff(data_root)
        .as_ref()
        .and_then(|value| value.get("handoff_id").and_then(Value::as_str))
        != Some(handoff_id)
    {
        return Ok(());
    }
    write_daemon_upgrade_handoff(data_root, handoff_id, "completed", None)
}

pub(crate) fn replacement_helper_owns_daemon_handoff(
    data_root: &Path,
    handoff_id: &str,
    helper_pid: u32,
) -> bool {
    read_daemon_upgrade_handoff(data_root).is_some_and(|value| {
        value.get("handoff_id").and_then(Value::as_str) == Some(handoff_id)
            && value.get("phase").and_then(Value::as_str) == Some("scheduled")
            && value
                .get("helper_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(helper_pid)
    })
}

pub(super) fn write_daemon_upgrade_handoff(
    data_root: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    write_daemon_upgrade_handoff_at(
        &daemon_upgrade_handoff_path(data_root),
        handoff_id,
        phase,
        helper_pid,
    )
}

fn write_daemon_upgrade_handoff_at(
    handoff_path: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    write_private_json_file(
        handoff_path,
        &compact_json(json!({
            "schema_version": 1,
            "handoff_id": handoff_id,
            "phase": phase,
            "owner_pid": process::id(),
            "helper_pid": helper_pid,
            "updated_at_ms": utc_now().timestamp_millis(),
        })),
    )
}

pub(in crate::semantic) fn write_daemon_restart_request(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> Result<PathBuf> {
    write_daemon_restart_request_at(
        &daemon_upgrade_restart_request_root(data_root),
        trigger.as_str(),
        request_id,
    )
}

fn write_daemon_restart_request_at(
    restart_request_root: &Path,
    persisted_restart_label: &str,
    request_id: &str,
) -> Result<PathBuf> {
    let path = restart_request_root.join(format!("{request_id}.json"));
    write_private_json_file(
        &path,
        &compact_json(json!({
            "schema_version": 1,
            "request_id": request_id,
            "trigger_command": persisted_restart_label,
            "requester_pid": process::id(),
            "requested_at_ms": utc_now().timestamp_millis(),
        })),
    )?;
    Ok(path)
}

pub(in crate::semantic) fn read_daemon_restart_request(
    data_root: &Path,
) -> Option<(PathBuf, DaemonTriggerCommandArg)> {
    let restart_request_root = daemon_upgrade_restart_request_root(data_root);
    let Ok(entries) = fs::read_dir(&restart_request_root) else {
        return None;
    };
    let mut paths = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(label) = value.get("trigger_command").and_then(Value::as_str) else {
            continue;
        };
        if let Some(trigger) = parse_daemon_trigger(Some(label)) {
            return Some((path, trigger));
        }
    }
    None
}

#[cfg(test)]
fn read_daemon_restart_request_at(restart_request_root: &Path) -> Option<(PathBuf, String)> {
    read_daemon_restart_requests_at(restart_request_root)
        .into_iter()
        .next()
}

#[cfg(test)]
fn read_daemon_restart_requests_at(restart_request_root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(restart_request_root) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = fs::read_to_string(&path).ok()?;
            let value: Value = serde_json::from_str(&text).ok()?;
            let label = value.get("trigger_command").and_then(Value::as_str)?;
            Some((path, label.to_owned()))
        })
        .collect()
}

pub(super) fn remove_daemon_restart_requests(data_root: &Path) {
    let root = daemon_upgrade_restart_request_root(data_root);
    if let Ok(entries) = fs::read_dir(&root) {
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir(root);
}

pub(in crate::semantic) fn acknowledge_daemon_restart_requests(data_root: &Path) {
    remove_daemon_restart_requests(data_root);
}

#[cfg(test)]
mod seam_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct ObservingStop {
        handoff_path: PathBuf,
        restart_root: PathBuf,
        calls: Arc<AtomicUsize>,
    }

    impl CooperativeStopPort for ObservingStop {
        fn request_stop(&mut self) {
            let handoff = read_daemon_upgrade_handoff_at(&self.handoff_path)
                .expect("handoff fence must precede cooperative stop");
            assert_eq!(handoff["phase"], "preparing");
            let (_, label) = read_daemon_restart_request_at(&self.restart_root)
                .expect("restart intent must precede cooperative stop");
            assert_eq!(label, "opaque-restart-label-v9");
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn opaque_restart_intent_and_fence_are_durable_before_cooperative_stop() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_path = temp.path().join("handoff.json");
        let restart_root = temp.path().join("restart");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut stop = ObservingStop {
            handoff_path: handoff_path.clone(),
            restart_root: restart_root.clone(),
            calls: Arc::clone(&calls),
        };

        persist_handoff_before_cooperative_stop(
            &handoff_path,
            &restart_root,
            "opaque-handoff-id",
            Some("opaque-restart-label-v9"),
            &mut stop,
        )?;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn cooperative_stop_is_not_called_when_handoff_persistence_fails() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let blocked_parent = temp.path().join("not-a-directory");
        fs::write(&blocked_parent, b"blocked")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut stop = ObservingStop {
            handoff_path: blocked_parent.join("handoff.json"),
            restart_root: temp.path().join("restart"),
            calls: Arc::clone(&calls),
        };

        assert!(persist_handoff_before_cooperative_stop(
            &stop.handoff_path.clone(),
            &stop.restart_root.clone(),
            "opaque-handoff-id",
            Some("opaque-restart-label-v9"),
            &mut stop,
        )
        .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn product_restart_reader_skips_unknown_opaque_labels_without_rewriting_schema() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let restart_root = daemon_upgrade_restart_request_root(temp.path());
        write_daemon_restart_request_at(
            &restart_root,
            "opaque-future-restart-label",
            "000-opaque",
        )?;
        write_daemon_restart_request_at(&restart_root, "search", "001-product")?;

        let (_, trigger) = read_daemon_restart_request(temp.path())
            .expect("product reader must continue past unknown opaque labels");
        assert_eq!(trigger.as_str(), "search");
        let (_, opaque) = read_daemon_restart_request_at(&restart_root)
            .expect("generic reader preserves the first opaque label");
        assert_eq!(opaque, "opaque-future-restart-label");
        Ok(())
    }

    #[test]
    fn fresh_corrupt_handoff_is_a_start_fence_not_an_absent_handoff() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_path = daemon_upgrade_handoff_path(temp.path());
        fs::create_dir_all(handoff_path.parent().expect("handoff parent"))?;
        fs::write(&handoff_path, b"{not-json")?;

        assert_eq!(
            daemon_upgrade_handoff_state_at(&handoff_path),
            DaemonUpgradeHandoffState::CorruptOrUnreadable
        );
        assert!(daemon_upgrade_handoff_fences_start(temp.path()));
        assert!(daemon_upgrade_handoff_blocks_current_process(temp.path()));
        Ok(())
    }

    #[test]
    fn restart_reader_returns_at_first_recognized_trigger_in_path_order() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let restart_root = daemon_upgrade_restart_request_root(temp.path());
        write_daemon_restart_request_at(&restart_root, "search", "000-first")?;
        // A directory here would make a full traversal perform another failed
        // file read. The selected first request must make it irrelevant.
        fs::create_dir(restart_root.join("001-never-read.json"))?;

        let (path, trigger) = read_daemon_restart_request(temp.path())
            .expect("first recognized trigger must be selected");
        assert_eq!(path.file_name().and_then(|name| name.to_str()), Some("000-first.json"));
        assert_eq!(trigger.as_str(), DaemonTriggerCommandArg::Search.as_str());
        Ok(())
    }
}
