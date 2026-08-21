use std::collections::BTreeMap;
#[cfg(windows)]
use std::ffi::OsString;

use super::*;
use serde_json::json;

#[test]
fn task_scheduler_probe_is_read_only() {
    let environment = SupervisorManagerEnvironment::new(BTreeMap::new());
    let command = windows_task_scheduler_probe_command(&environment);
    assert_eq!(command.get_program(), "schtasks");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["/Query", "/FO", "CSV", "/NH"]
            .iter()
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>()
    );
}

#[test]
fn current_windows_sid_uses_the_process_token_without_a_path_command() {
    let supervisor = include_str!("../windows.rs");
    let identity = include_str!("../../windows_identity.rs");
    let pipe_security = include_str!("../../ipc/server/windows_security.rs");

    assert!(!supervisor.contains("whoami"));
    assert!(!identity.contains("Command::new"));
    assert!(!identity.contains("\"whoami\""));
    assert!(identity.contains("OpenProcessToken(GetCurrentProcess()"));
    assert!(identity.contains("GetTokenInformation"));
    assert!(identity.contains("TokenUser"));
    assert!(identity.contains("ConvertSidToStringSidW"));
    assert!(identity.contains("impl Drop for LocalSidString"));
    assert!(identity.contains("LocalFree"));
    assert!(pipe_security.contains("CurrentProcessTokenUser::current()"));
    assert!(!pipe_security.contains("GetTokenInformation"));
}

#[test]
fn task_state_query_represents_absence_separately_from_failure() {
    let script = windows_task_state_script(r"\ctx-test-task");
    assert!(script.contains("-ErrorAction Stop"));
    assert!(script.contains("Where-Object {$_.TaskName -eq 'ctx-test-task'}"));
    assert!(script.contains("Write('absent')"));
    assert_eq!(parse_windows_task_state_query(b"absent"), Some(None));
    assert_eq!(parse_windows_task_state_query(b"4"), Some(Some(4)));
    assert_eq!(parse_windows_task_state_query(b"not-a-state"), None);
}

#[test]
fn task_deletion_requires_a_successful_absence_query() {
    verify_windows_task_deletion(false, b"task is absent", Ok(None)).unwrap();

    let query_error =
        verify_windows_task_deletion(true, b"", Err(anyhow!("Task Scheduler RPC unavailable")))
            .unwrap_err();
    assert!(
        format!("{query_error:#}").contains("Task Scheduler RPC unavailable"),
        "{query_error:#}"
    );

    let delete_error =
        verify_windows_task_deletion(false, b"access denied", Ok(Some(3))).unwrap_err();
    assert!(format!("{delete_error:#}").contains("access denied"));

    let surviving_task = verify_windows_task_deletion(true, b"", Ok(Some(3))).unwrap_err();
    assert!(format!("{surviving_task:#}").contains("remained registered"));
}

#[cfg(windows)]
#[test]
fn disabling_an_already_absent_real_task_is_idempotent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let task_name = format!(r"\ctx-absent-disable-test-{}", std::process::id());
    let artifact = temp.path().join("ctx-task.xml");
    let identity = SupervisorIdentity::new(task_name, artifact.clone())?;
    fs::write(&artifact, b"stale task definition")?;
    fs::write(
        windows_supervisor_owner_provenance_path(&identity)?,
        b"stale owner provenance",
    )?;
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable in test"))?;
    let manager_environment = SupervisorManagerEnvironment::new(BTreeMap::from([(
        OsString::from("SystemRoot"),
        system_root.clone(),
    )]));

    assert_eq!(
        windows_task_state(
            identity.name(),
            Path::new(&system_root),
            &manager_environment,
        )?,
        None,
    );
    assert_eq!(
        disable_windows_supervisor(&identity, &manager_environment)?,
        Some(artifact.clone()),
    );
    assert!(!artifact.exists());
    assert!(!windows_supervisor_owner_provenance_path(&identity)?.exists());
    assert_eq!(
        disable_windows_supervisor(&identity, &manager_environment)?,
        Some(artifact),
    );
    Ok(())
}

#[test]
fn windows_manager_ownership_rejects_same_binary_detached_fallback() {
    let detached_fallback_lock = json!({
        "lock_protocol": "advisory-v1",
        "owner_id": "fallback-owner",
        "pid": 4242,
        "binary": r"C:\Program Files\ctx\ctx.exe",
        "binary_sha256": "same-binary-image",
    });
    let stale_manager_provenance = json!({
        "schema_version": 1,
        "owner_id": "previous-manager-owner",
        "pid": 4242,
    });
    assert!(!windows_supervisor_owner_provenance_matches(
        &detached_fallback_lock,
        &stale_manager_provenance,
    ));

    let current_manager_provenance = json!({
        "schema_version": 1,
        "owner_id": "fallback-owner",
        "pid": 4242,
    });
    assert!(windows_supervisor_owner_provenance_matches(
        &detached_fallback_lock,
        &current_manager_provenance,
    ));
    assert!(!windows_supervisor_owner_provenance_matches(
        &detached_fallback_lock,
        &json!({"schema_version": 1, "pid": 4242}),
    ));
}
