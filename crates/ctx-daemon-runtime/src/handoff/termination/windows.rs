#[cfg(test)]
use std::{fs, path::PathBuf};
use std::{os::windows::ffi::OsStrExt as _, path::Path, process, ptr};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use windows_sys::Win32::{
    Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, HANDLE},
    System::{
        RestartManager::{
            RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
            RM_PROCESS_INFO,
        },
        Threading::TerminateProcess,
    },
};

use super::super::DAEMON_UPGRADE_RESTART_TIMEOUT;
use crate::{
    daemon_lock_path, observe_pid_advisory_lock, pid_from_lock_json, pid_lock_guard_path,
    read_pid_lock_json, PidAdvisoryLockObservation, PID_LOCK_PROTOCOL,
};

mod legacy_image;
mod process_handle;
use legacy_image::{
    verify_lock_paths, verify_recorded_digest_identity, LegacyProcessImageProof,
    OFFICIAL_V025_WINDOWS_X64_SHA256,
};
use process_handle::{filetime_unix_ms, filetime_value, WindowsProcess, WindowsProcessAccess};

const LEGACY_V025_FIELDS: [&str; 7] = [
    "binary",
    "data_root",
    "lock_protocol",
    "owner_id",
    "pid",
    "released",
    "started_at_ms",
];
const LEGACY_PROCESS_START_MAX_DELAY_MS: i64 = 5 * 60 * 1_000;
const LEGACY_PROCESS_START_CLOCK_SKEW_MS: i64 = 5_000;
const MAX_RESTART_MANAGER_PROCESSES: u32 = 256;

pub fn terminate_identity_verified_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    terminate_identity_verified_residual_daemon_owner(data_root, expected_executable, None)
}

pub fn terminate_identity_verified_residual_daemon_owner(
    data_root: &Path,
    expected_executable: &Path,
    expected_owner_id: Option<&str>,
) -> Result<()> {
    terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
        data_root,
        expected_executable,
        expected_owner_id,
        OFFICIAL_V025_WINDOWS_X64_SHA256,
    )
}

fn terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
    data_root: &Path,
    expected_executable: &Path,
    expected_owner_id: Option<&str>,
    legacy_sha256: &str,
) -> Result<()> {
    let lock_path = daemon_lock_path(data_root);
    let value = read_pid_lock_json(&lock_path)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no process identity"))?;
    if pid == process::id() {
        return Err(anyhow!("refusing to terminate the current ctx process"));
    }
    let observed_owner_id = value.get("owner_id").and_then(Value::as_str);
    if expected_owner_id.is_some() && observed_owner_id != expected_owner_id {
        return Err(anyhow!(
            "ctx daemon ownership changed after health verification; refusing to terminate"
        ));
    }
    let legacy_v025 = value.get("binary_sha256").is_none();
    verify_lock_paths(&value, data_root, expected_executable)?;
    if legacy_v025 {
        verify_exact_legacy_v025_lock(&value, true)?;
    }

    let owner_released = value.get("released").and_then(Value::as_bool) == Some(true);

    let access = if owner_released {
        WindowsProcessAccess::Observe
    } else if legacy_v025 {
        WindowsProcessAccess::LegacyTerminate
    } else {
        WindowsProcessAccess::ModernTerminate
    };
    let Some(target) = WindowsProcess::open(pid, access)? else {
        if advisory_lock_is_held(data_root) {
            return Err(anyhow!(
                "ctx daemon owner lock is held but its recorded process is not running"
            ));
        }
        return Ok(());
    };
    if owner_released {
        return wait_for_released_process(target, &value);
    }
    match observe_pid_advisory_lock(&lock_path) {
        Some(PidAdvisoryLockObservation { held: false, .. }) => {
            return wait_for_released_process(target, &value);
        }
        Some(PidAdvisoryLockObservation {
            held: true,
            released: false,
        }) => {}
        None => {
            return Err(anyhow!(
                "ctx daemon owner lock state is unreadable; refusing residual termination"
            ));
        }
        Some(PidAdvisoryLockObservation {
            held: true,
            released: true,
        }) => {
            if let Some(current) = read_pid_lock_json(&lock_path) {
                if is_same_owner_release_transition(&value, &current) {
                    return wait_for_released_process(target, &current);
                }
            }
            return Err(anyhow!(
                "ctx daemon owner lock state is inconsistent; refusing residual termination"
            ));
        }
    }

    let mut legacy_image_proof = None;
    if legacy_v025 {
        verify_legacy_process_start(&value, &target, false)?;
        if verify_legacy_guard_owner(data_root, &target)? == LegacyGuardOwnership::Released {
            return wait_for_released_process(target, &value);
        }
        legacy_image_proof = Some(LegacyProcessImageProof::verify(
            &target,
            expected_executable,
            legacy_sha256,
        )?);
    } else {
        verify_recorded_digest_identity(pid, &value)?;
    }

    if let Some(proof) = legacy_image_proof.as_mut() {
        if !proof.recheck(&target, expected_executable)? {
            return Ok(());
        }
        #[cfg(test)]
        legacy_image::release_guard_after_image_proof_for_test(data_root)?;
    }
    let owner_id = expected_owner_id.or(observed_owner_id);
    if let OwnerMetadataStatus::Released(current) =
        recheck_owner_metadata(&lock_path, &value, pid, owner_id)?
    {
        return wait_for_released_process(target, &current);
    }
    if legacy_v025 {
        if verify_legacy_guard_owner(data_root, &target)? == LegacyGuardOwnership::Released {
            return wait_for_released_process(target, &value);
        }
    } else if !advisory_lock_is_held(data_root) {
        return wait_for_released_process(target, &value);
    }
    if let OwnerMetadataStatus::Released(current) =
        recheck_owner_metadata(&lock_path, &value, pid, owner_id)?
    {
        return wait_for_released_process(target, &current);
    }
    if !target.is_running()? {
        return Ok(());
    }
    terminate_process_and_wait(&target)
}

pub fn wait_for_released_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    let lock_path = daemon_lock_path(data_root);
    let Some(value) = read_pid_lock_json(&lock_path) else {
        return Ok(());
    };
    if value.get("lock_protocol").and_then(Value::as_str) != Some(PID_LOCK_PROTOCOL) {
        return Ok(());
    }
    let Some(observation) = observe_pid_advisory_lock(&lock_path) else {
        return Err(anyhow!(
            "ctx daemon owner lock state is unreadable after cooperative shutdown"
        ));
    };
    if observation.held && !observation.released {
        return Ok(());
    }
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("released ctx daemon lock has no process identity"))?;
    if pid == process::id() {
        return Err(anyhow!(
            "released ctx daemon lock names the current process"
        ));
    }
    let Some(target) = WindowsProcess::open(pid, WindowsProcessAccess::Observe)? else {
        return Ok(());
    };
    match verify_process_start_for_released_lock(&value, &target)? {
        ReleasedProcessIdentity::OriginalOwner => {}
        ReleasedProcessIdentity::ReusedPid => return Ok(()),
    }
    verify_lock_paths(&value, data_root, expected_executable)?;
    wait_for_released_process(target, &value)
}

pub fn terminate_identity_verified_legacy_daemon(
    _data_root: &Path,
    _expected_executable: &Path,
) -> Result<()> {
    Err(anyhow!(
        "legacy automatic daemon replacement is not supported on Windows"
    ))
}

fn verify_exact_legacy_v025_lock(value: &Value, allow_released: bool) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Err(anyhow!("legacy ctx daemon lock is not a JSON object"));
    };
    if object.len() != LEGACY_V025_FIELDS.len()
        || !LEGACY_V025_FIELDS
            .iter()
            .all(|field| object.contains_key(*field))
    {
        return Err(anyhow!(
            "digest-free ctx daemon lock does not match the exact v0.25 ownership schema"
        ));
    }
    if value.get("lock_protocol").and_then(Value::as_str) != Some(PID_LOCK_PROTOCOL) {
        return Err(anyhow!(
            "legacy ctx daemon lock does not use the v0.25 advisory protocol"
        ));
    }
    if !value
        .get("owner_id")
        .and_then(Value::as_str)
        .is_some_and(|owner| !owner.is_empty())
    {
        return Err(anyhow!("legacy ctx daemon lock has no owner identity"));
    }
    if pid_from_lock_json(value).is_none() {
        return Err(anyhow!("legacy ctx daemon lock has no process identity"));
    }
    let released = value.get("released").and_then(Value::as_bool);
    if released.is_none() || (!allow_released && released != Some(false)) {
        return Err(anyhow!("legacy ctx daemon lock is not live owner metadata"));
    }
    if !value
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .is_some_and(|started| started > 0)
    {
        return Err(anyhow!(
            "legacy ctx daemon lock has no process-start identity"
        ));
    }
    if !["binary", "data_root"].iter().all(|field| {
        value
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|path| !path.is_empty())
    }) {
        return Err(anyhow!(
            "legacy ctx daemon lock has incomplete path identity"
        ));
    }
    Ok(())
}

fn is_same_owner_release_transition(before: &Value, after: &Value) -> bool {
    if before.get("released").and_then(Value::as_bool) != Some(false)
        || after.get("released").and_then(Value::as_bool) != Some(true)
    {
        return false;
    }
    let mut expected = before.clone();
    let Some(object) = expected.as_object_mut() else {
        return false;
    };
    object.insert("released".to_owned(), Value::Bool(true));
    expected == *after
}

enum OwnerMetadataStatus {
    Unchanged,
    Released(Value),
}

fn recheck_owner_metadata(
    lock_path: &Path,
    original: &Value,
    pid: u32,
    owner_id: Option<&str>,
) -> Result<OwnerMetadataStatus> {
    let current = read_pid_lock_json(lock_path)
        .ok_or_else(|| anyhow!("ctx daemon ownership disappeared before termination"))?;
    if pid_from_lock_json(&current) != Some(pid)
        || owner_id.is_some_and(|expected| {
            current.get("owner_id").and_then(Value::as_str) != Some(expected)
        })
    {
        return Err(anyhow!(
            "ctx daemon ownership changed before termination; refusing to terminate"
        ));
    }
    if current == *original {
        return Ok(OwnerMetadataStatus::Unchanged);
    }
    if is_same_owner_release_transition(original, &current) {
        return Ok(OwnerMetadataStatus::Released(current));
    }
    Err(anyhow!(
        "ctx daemon ownership metadata changed before termination; refusing to terminate"
    ))
}

fn verify_legacy_process_start(
    value: &Value,
    target: &WindowsProcess,
    allow_reused: bool,
) -> Result<ReleasedProcessIdentity> {
    let started_at_ms = value
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("ctx daemon lock has no process-start identity"))?;
    let creation_ms = filetime_unix_ms(target.creation_time).ok_or_else(|| {
        anyhow!(
            "ctx daemon process {} has an invalid creation identity",
            target.pid
        )
    })?;
    if creation_ms > started_at_ms.saturating_add(LEGACY_PROCESS_START_CLOCK_SKEW_MS) {
        if allow_reused {
            return Ok(ReleasedProcessIdentity::ReusedPid);
        }
        return Err(anyhow!(
            "legacy ctx daemon PID was reused after its lock was published; refusing to terminate"
        ));
    }
    if started_at_ms.saturating_sub(creation_ms) > LEGACY_PROCESS_START_MAX_DELAY_MS {
        return Err(anyhow!(
            "legacy ctx daemon lock timestamp does not bind its recorded process; refusing to terminate"
        ));
    }
    Ok(ReleasedProcessIdentity::OriginalOwner)
}

fn verify_process_start_for_released_lock(
    value: &Value,
    target: &WindowsProcess,
) -> Result<ReleasedProcessIdentity> {
    if value.get("binary_sha256").is_none() {
        verify_exact_legacy_v025_lock(value, true)?;
    } else if value.get("binary_sha256").and_then(Value::as_str).is_none() {
        return Err(anyhow!(
            "released ctx daemon lock has an invalid executable digest identity"
        ));
    }
    verify_legacy_process_start(value, target, true)
}

fn wait_for_released_process(target: WindowsProcess, value: &Value) -> Result<()> {
    match verify_process_start_for_released_lock(value, &target)? {
        ReleasedProcessIdentity::OriginalOwner => {}
        ReleasedProcessIdentity::ReusedPid => return Ok(()),
    }
    target
        .wait_for_exit(DAEMON_UPGRADE_RESTART_TIMEOUT)
        .context("wait for released ctx daemon owner process to exit")
}

fn terminate_process_and_wait(target: &WindowsProcess) -> Result<()> {
    terminate_process_and_wait_with(target, |handle| {
        if unsafe { TerminateProcess(handle, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })
}

fn terminate_process_and_wait_with(
    target: &WindowsProcess,
    terminate: impl FnOnce(HANDLE) -> std::io::Result<()>,
) -> Result<()> {
    if let Err(error) = terminate(target.handle) {
        if target.is_running().is_ok_and(|running| !running) {
            return Ok(());
        }
        return Err(error).context("terminate identity-verified residual ctx daemon");
    }
    target
        .wait_for_exit(DAEMON_UPGRADE_RESTART_TIMEOUT)
        .context("wait for terminated residual ctx daemon process to exit")
}

fn verify_legacy_guard_owner(
    data_root: &Path,
    target: &WindowsProcess,
) -> Result<LegacyGuardOwnership> {
    if !advisory_lock_is_held(data_root) {
        return Ok(LegacyGuardOwnership::Released);
    }
    if legacy_guard_is_owned_by(data_root, target)? {
        return Ok(LegacyGuardOwnership::Owned);
    }
    if !advisory_lock_is_held(data_root) {
        return Ok(LegacyGuardOwnership::Released);
    }
    Err(anyhow!(
        "legacy ctx daemon PID does not own its advisory guard lock; refusing to terminate"
    ))
}

fn legacy_guard_is_owned_by(data_root: &Path, target: &WindowsProcess) -> Result<bool> {
    let guard_path = pid_lock_guard_path(&daemon_lock_path(data_root));
    let holders = restart_manager_processes_using(&guard_path)?;
    Ok(holders.len() == 1
        && holders[0].Process.dwProcessId == target.pid
        && filetime_value(holders[0].Process.ProcessStartTime) == target.creation_time)
}

fn restart_manager_processes_using(path: &Path) -> Result<Vec<RM_PROCESS_INFO>> {
    let session = RestartManagerSession::start()?;
    let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    path_wide.push(0);
    let resources = [path_wide.as_ptr()];
    let registered = unsafe {
        RmRegisterResources(
            session.0,
            1,
            resources.as_ptr(),
            0,
            ptr::null(),
            0,
            ptr::null(),
        )
    };
    require_win32_success(registered, "register ctx daemon guard with Restart Manager")?;

    let mut entries = Vec::<RM_PROCESS_INFO>::new();
    for _ in 0..3 {
        let mut needed = 0;
        let mut count =
            u32::try_from(entries.len()).context("bound Restart Manager process list")?;
        let mut reboot_reasons = 0;
        let status = unsafe {
            RmGetList(
                session.0,
                &raw mut needed,
                &raw mut count,
                if entries.is_empty() {
                    ptr::null_mut()
                } else {
                    entries.as_mut_ptr()
                },
                &raw mut reboot_reasons,
            )
        };
        if status == ERROR_SUCCESS {
            entries.truncate(usize::try_from(count).context("size Restart Manager process list")?);
            return Ok(entries);
        }
        if status != ERROR_MORE_DATA || needed == 0 || needed > MAX_RESTART_MANAGER_PROCESSES {
            return Err(win32_error(status)).context("enumerate processes using ctx daemon guard");
        }
        entries.resize(
            usize::try_from(needed).context("size Restart Manager process list")?,
            RM_PROCESS_INFO::default(),
        );
    }
    Err(anyhow!(
        "ctx daemon guard ownership changed repeatedly during verification"
    ))
}

struct RestartManagerSession(u32);

impl RestartManagerSession {
    fn start() -> Result<Self> {
        let mut handle = 0;
        let mut session_key = vec![0_u16; usize::try_from(CCH_RM_SESSION_KEY).unwrap_or(32) + 1];
        let status = unsafe { RmStartSession(&raw mut handle, 0, session_key.as_mut_ptr()) };
        require_win32_success(status, "start Restart Manager ownership query")?;
        Ok(Self(handle))
    }
}

impl Drop for RestartManagerSession {
    fn drop(&mut self) {
        unsafe {
            RmEndSession(self.0);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleasedProcessIdentity {
    OriginalOwner,
    ReusedPid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyGuardOwnership {
    Owned,
    Released,
}

fn advisory_lock_is_held(data_root: &Path) -> bool {
    observe_pid_advisory_lock(&daemon_lock_path(data_root)).is_some_and(|state| state.held)
}

fn require_win32_success(status: u32, context: &'static str) -> Result<()> {
    if status == ERROR_SUCCESS {
        return Ok(());
    }
    Err(win32_error(status)).context(context)
}

fn win32_error(status: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        process::{Child, Command, Stdio},
        sync::{Mutex, MutexGuard},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use fs2::FileExt;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        open_or_create_pid_lock_file, publish_pid_lock_metadata, secure_private_file_permissions,
        try_lock_pid_file,
    };

    pub(super) const CHILD_TEST: &str =
        "handoff::termination::windows::tests::legacy_v025_owner_child";
    pub(super) const CHILD_MODE_ENV: &str = "CTX_TEST_LEGACY_V025_CHILD_MODE";
    pub(super) const CHILD_ROOT_ENV: &str = "CTX_TEST_LEGACY_V025_ROOT";
    pub(super) const CHILD_SHA_ENV: &str = "CTX_TEST_LEGACY_V025_SHA256";
    pub(super) const CHILD_EXPECT_ERROR_ENV: &str = "CTX_TEST_LEGACY_V025_EXPECT_ERROR";
    pub(super) const CHILD_OPEN_IMAGE_ENV: &str = "CTX_TEST_LEGACY_V025_OPEN_IMAGE";
    static FIXTURE_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(super) struct LegacyFixture {
        _temp: TempDir,
        pub(super) active: PathBuf,
        pub(super) root: PathBuf,
        pub(super) owner: Child,
    }

    impl LegacyFixture {
        pub(super) fn start() -> Self {
            let temp = tempfile::tempdir().expect("temporary legacy fixture");
            let active = temp.path().join("ctx.exe");
            let root = temp.path().join("data");
            fs::create_dir_all(&root).expect("create legacy data root");
            fs::copy(
                env::current_exe().expect("current test executable"),
                &active,
            )
            .expect("copy legacy fixture executable");
            let owner = spawn_fixture_child(&active, &root, "owner");
            wait_for_path(&root.join("owner-ready"));
            assert_eq!(
                observe_pid_advisory_lock(&daemon_lock_path(&root)),
                Some(PidAdvisoryLockObservation {
                    held: true,
                    released: false,
                }),
                "legacy fixture readiness did not publish a held advisory owner"
            );
            Self {
                _temp: temp,
                active,
                root,
                owner,
            }
        }
    }

    impl Drop for LegacyFixture {
        fn drop(&mut self) {
            if self.owner.try_wait().ok().flatten().is_none() {
                let _ = self.owner.kill();
            }
            let _ = self.owner.wait();
        }
    }

    #[test]
    fn exact_v025_schema_is_the_only_digest_free_termination_schema() {
        let value = legacy_lock_value(
            Path::new(r"C:\ctx-data"),
            Path::new(r"C:\bin\ctx.exe"),
            42,
            1,
            false,
        );
        verify_exact_legacy_v025_lock(&value, false).expect("exact v0.25 schema");

        let mut digest_field = value.clone();
        digest_field["binary_sha256"] = Value::Null;
        assert!(verify_exact_legacy_v025_lock(&digest_field, false).is_err());

        let mut extra_field = value.clone();
        extra_field["unexpected"] = Value::Bool(true);
        assert!(verify_exact_legacy_v025_lock(&extra_field, false).is_err());

        let mut released = value.clone();
        released["released"] = Value::Bool(true);
        assert!(verify_exact_legacy_v025_lock(&released, false).is_err());
        verify_exact_legacy_v025_lock(&released, true).expect("released retry metadata");
        assert!(is_same_owner_release_transition(&value, &released));

        let mut changed_owner = released;
        changed_owner["owner_id"] = Value::String("different-owner".to_owned());
        assert!(!is_same_owner_release_transition(&value, &changed_owner));
    }

    #[test]
    fn forced_legacy_termination_returns_only_after_process_exit() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open legacy owner signal handle")
            .expect("live legacy owner");

        let sha256 = crate::executable_sha256(&fixture.active).expect("fixture image digest");
        terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
            &fixture.root,
            &fixture.active,
            None,
            &sha256,
        )
        .expect("terminate guard-bound legacy owner");
        assert!(
            !target.is_running().expect("inspect legacy owner signal"),
            "residual termination returned before its process handle was signaled"
        );
        assert!(
            fixture
                .owner
                .try_wait()
                .expect("inspect terminated legacy owner")
                .is_some(),
            "residual termination returned before the child exited"
        );
        assert!(
            !fixture.root.join("clean-exit").exists(),
            "identity-verified residual termination unexpectedly used the clean-exit path"
        );
    }

    #[test]
    fn legacy_fallback_rejects_an_unrelated_same_path_process() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let mut unrelated = spawn_fixture_child(&fixture.active, &fixture.root, "idle");
        wait_for_path(&fixture.root.join("idle-ready"));

        let value = legacy_lock_value(
            &fixture.root,
            &fixture.active,
            unrelated.id(),
            unix_now_ms(),
            false,
        );
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode spoofed legacy lock"),
        )
        .expect("publish spoofed legacy lock");

        let error = terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect_err("unrelated process must not be terminated");
        assert!(
            error
                .to_string()
                .contains("does not own its advisory guard lock"),
            "{error:#}"
        );
        assert!(fixture.owner.try_wait().expect("inspect owner").is_none());
        assert!(unrelated.try_wait().expect("inspect unrelated").is_none());
        unrelated.kill().expect("stop unrelated fixture");
        unrelated.wait().expect("join unrelated fixture");
    }

    #[test]
    fn digest_bearing_metadata_never_uses_the_legacy_fallback() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let mut value =
            read_pid_lock_json(&daemon_lock_path(&fixture.root)).expect("legacy fixture metadata");
        value["binary_sha256"] = Value::String("0".repeat(64));
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode digest-bearing lock"),
        )
        .expect("publish digest-bearing lock");

        let error = terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect_err("digest mismatch must not downgrade to legacy verification");
        assert!(
            error
                .to_string()
                .contains("owner image does not match its held ctx daemon lock"),
            "{error:#}"
        );
        assert!(fixture.owner.try_wait().expect("inspect owner").is_none());
    }

    #[test]
    fn digest_bearing_owner_terminates_with_modern_process_rights() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open modern owner signal handle")
            .expect("live modern owner");
        let mut value =
            read_pid_lock_json(&daemon_lock_path(&fixture.root)).expect("owner metadata");
        value["binary_sha256"] = Value::String(
            crate::executable_sha256(&fixture.active).expect("modern fixture digest"),
        );
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode digest-bearing lock"),
        )
        .expect("publish digest-bearing lock");

        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("terminate digest-bound modern owner");
        assert!(
            !target.is_running().expect("inspect modern owner signal"),
            "modern residual termination returned before process exit"
        );
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect modern owner")
            .is_some());
    }

    #[test]
    fn released_metadata_while_guard_is_held_waits_for_clean_exit() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open releasing legacy owner signal handle")
            .expect("live releasing legacy owner");
        fs::write(fixture.root.join("release-trigger"), b"release")
            .expect("trigger legacy release publication");
        wait_for_path(&fixture.root.join("release-published"));
        assert_eq!(
            observe_pid_advisory_lock(&daemon_lock_path(&fixture.root)),
            Some(PidAdvisoryLockObservation {
                held: true,
                released: true,
            }),
            "fixture did not publish released metadata while retaining its guard"
        );

        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("wait for releasing legacy owner");
        assert!(!target.is_running().expect("inspect releasing owner"));
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect clean releasing owner")
            .expect("releasing owner did not exit before return");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());
    }

    #[test]
    fn released_guard_with_true_metadata_waits_for_clean_exit_and_retry_is_idempotent() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open released legacy owner signal handle")
            .expect("live released legacy owner");
        fs::write(fixture.root.join("release-trigger"), b"release")
            .expect("trigger legacy guard release");
        wait_for_path(&fixture.root.join("guard-released"));
        let released = read_pid_lock_json(&daemon_lock_path(&fixture.root))
            .expect("released legacy fixture metadata");
        assert_eq!(released["released"], true, "{released:#}");

        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("wait for released legacy owner");
        assert!(
            !target.is_running().expect("inspect released owner signal"),
            "released-owner wait returned before its process handle was signaled"
        );
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect clean legacy owner")
            .expect("released-owner wait returned before the child exited");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());

        wait_for_released_residual_daemon(&fixture.root, &fixture.active)
            .expect("released-owner retry");
    }

    #[test]
    fn natural_exit_after_running_check_preserves_success_on_the_same_handle() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target =
            WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::LegacyTerminate)
                .expect("open natural-exit fixture handle")
                .expect("live natural-exit fixture");
        assert!(target.is_running().expect("initial running check"));

        let error =
            terminate_process_and_wait_with(&target, |_| Err(std::io::Error::from_raw_os_error(5)))
                .expect_err("a failed termination of a live process must remain an error");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(5),
            "termination failure did not preserve its original OS error: {error:#}"
        );
        assert!(target
            .is_running()
            .expect("running after failed termination"));

        terminate_process_and_wait_with(&target, |_| {
            fixture.owner.kill()?;
            fixture.owner.wait()?;
            Err(std::io::Error::from_raw_os_error(5))
        })
        .expect("natural exit after the running check");
        assert!(
            !target.is_running().expect("inspect natural-exit handle"),
            "natural-exit recovery accepted an unsignaled process handle"
        );
    }

    #[test]
    fn legacy_v025_owner_child() {
        let Some(mode) = env::var_os(CHILD_MODE_ENV) else {
            return;
        };
        let root = PathBuf::from(env::var_os(CHILD_ROOT_ENV).expect("legacy child root"));
        if mode == "takeover" {
            let expected = env::current_exe().expect("takeover child executable");
            let sha256 = env::var(CHILD_SHA_ENV)
                .unwrap_or_else(|_| OFFICIAL_V025_WINDOWS_X64_SHA256.to_owned());
            let result = terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
                &root, &expected, None, &sha256,
            );
            if let Ok(expected_error) = env::var(CHILD_EXPECT_ERROR_ENV) {
                let error = result.expect_err("legacy takeover unexpectedly succeeded");
                assert!(format!("{error:#}").contains(&expected_error), "{error:#}");
            } else {
                result.expect("legacy takeover child");
            }
            return;
        }
        if mode == "idle" {
            fs::write(root.join("idle-ready"), b"ready").expect("publish idle readiness");
            thread::sleep(Duration::from_secs(30));
            return;
        }
        let _opened_image = env::var_os(CHILD_OPEN_IMAGE_ENV)
            .map(|path| fs::File::open(path).expect("open unrelated legacy image fixture"));

        let daemon_root = root.join("daemon");
        fs::create_dir_all(&daemon_root).expect("create legacy daemon root");
        let guard_path = daemon_root.join("daemon.guard");
        let (guard, _) = open_or_create_pid_lock_file(&guard_path).expect("open legacy guard");
        secure_private_file_permissions(&guard_path).expect("secure legacy guard");
        assert!(
            try_lock_pid_file(&guard).expect("hold legacy advisory guard"),
            "legacy fixture guard unexpectedly contended"
        );
        let lock_path = daemon_root.join("daemon.lock");
        let value = legacy_lock_value(
            &root,
            &env::current_exe().expect("legacy child executable"),
            std::process::id(),
            unix_now_ms(),
            false,
        );
        assert!(
            publish_pid_lock_metadata(&lock_path, &value).expect("publish legacy child lock"),
            "legacy child lock publication was rejected"
        );
        fs::write(root.join("owner-ready"), b"ready").expect("publish owner readiness");

        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("release-trigger").exists() {
            assert!(
                Instant::now() < deadline,
                "legacy child exceeded its test lease"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let mut released = read_pid_lock_json(&lock_path).expect("read owner metadata to release");
        released["released"] = Value::Bool(true);
        assert!(
            publish_pid_lock_metadata(&lock_path, &released)
                .expect("publish released owner metadata"),
            "released owner metadata publication was rejected"
        );
        fs::write(root.join("release-published"), b"released")
            .expect("publish released-metadata readiness");
        thread::sleep(Duration::from_secs(1));
        FileExt::unlock(&guard).expect("release legacy advisory guard");
        drop(guard);
        fs::write(root.join("guard-released"), b"released").expect("publish guard release");
        thread::sleep(Duration::from_millis(250));
        fs::write(root.join("clean-exit"), b"clean").expect("publish clean exit");
    }

    pub(super) fn spawn_fixture_child(binary: &Path, root: &Path, mode: &str) -> Child {
        Command::new(binary)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_MODE_ENV, mode)
            .env(CHILD_ROOT_ENV, root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn legacy fixture child")
    }

    pub(super) fn fixture_test_guard() -> MutexGuard<'static, ()> {
        FIXTURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn legacy_lock_value(
        root: &Path,
        binary: &Path,
        pid: u32,
        started_at_ms: i64,
        released: bool,
    ) -> Value {
        json!({
            "lock_protocol": "advisory-v1",
            "owner_id": format!("v025-fixture-{pid}"),
            "pid": pid,
            "released": released,
            "started_at_ms": started_at_ms,
            "binary": binary,
            "data_root": root,
        })
    }

    pub(super) fn unix_now_ms() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_millis(),
        )
        .expect("current time fits i64 milliseconds")
    }

    pub(super) fn wait_for_path(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
