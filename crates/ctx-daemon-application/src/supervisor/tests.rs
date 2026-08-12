use super::*;

struct TestSupervisorUpgradeFence<F: FnOnce() -> Result<()>>(Option<F>);

impl<F: FnOnce() -> Result<()>> DaemonSupervisorUpgradeFence for TestSupervisorUpgradeFence<F> {
    fn release(&mut self) -> Result<()> {
        self.0
            .take()
            .ok_or_else(|| anyhow!("test supervisor upgrade fence released twice"))?()
    }
}
use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier, Mutex,
    },
};

const SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE: &str = "CTX_SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE";
const SUPERVISOR_ENV_ARTIFACT_PROBE_TEST: &str = "semantic::daemon_supervisor::tests::native_supervisor_artifacts_exclude_authority_and_fail_closed_on_controls";

fn linux_systemd_unit(executable: &Path, data_root: &Path) -> Result<String> {
    environment::linux_systemd_unit_with_environment(
        executable,
        data_root,
        &supervisor_environment_snapshot(&TestHost)?,
    )
}

fn launch_agent_plist(executable: &Path, data_root: &Path) -> Result<String> {
    environment::launch_agent_plist_with_environment(
        executable,
        data_root,
        &supervisor_environment_snapshot(&TestHost)?,
    )
}

fn windows_sanitized_daemon_script(executable: &Path, data_root: &Path) -> Result<String> {
    windows_sanitized_daemon_script_with_environment(
        executable,
        data_root,
        &supervisor_environment_snapshot(&TestHost)?,
    )
}

fn windows_task_xml(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> Result<String> {
    windows_task_xml_with_environment(
        executable,
        data_root,
        system_root,
        user_sid,
        task_name,
        &supervisor_environment_snapshot(&TestHost)?,
    )
}

fn windows_task_registration_matches(
    xml: &str,
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> Result<bool> {
    let input = ManagedSupervisorInput::new(&TestHost, data_root, executable)?;
    windows_task_registration_matches_with_environment(
        xml,
        system_root,
        user_sid,
        task_name,
        &input,
    )
}

#[derive(Default)]
struct FakeSupervisorState {
    manager_probes: usize,
    manager_unavailable: bool,
    mutation_preparations: usize,
    registered: bool,
    registration_probes: usize,
    live_owner: Option<u32>,
    installs: usize,
    disables: usize,
    starts: usize,
    upgrade_fence_released: bool,
    start_observed_released_fence: bool,
}

#[derive(Default)]
struct FakeSupervisorBackend {
    state: Mutex<FakeSupervisorState>,
    artifact_path_override: Option<PathBuf>,
    delay_install: bool,
    fail_install_after_registration: bool,
    fail_install_without_registration: bool,
    fail_disable: bool,
    manager_unavailable_after_install: bool,
    manager_unavailable_on_disable_failure: bool,
    manager_probe_error: bool,
    mutation_preparation_error: bool,
    fail_start: bool,
}

impl FakeSupervisorBackend {
    fn with_registration(live_owner: Option<u32>) -> Self {
        Self {
            state: Mutex::new(FakeSupervisorState {
                registered: true,
                live_owner,
                ..FakeSupervisorState::default()
            }),
            artifact_path_override: None,
            delay_install: false,
            fail_install_after_registration: false,
            fail_install_without_registration: false,
            fail_disable: false,
            manager_unavailable_after_install: false,
            manager_unavailable_on_disable_failure: false,
            manager_probe_error: false,
            mutation_preparation_error: false,
            fail_start: false,
        }
    }
}

impl NativeSupervisorBackend<SupervisorEnvironmentSnapshot> for FakeSupervisorBackend {
    fn probe_manager(&self, _data_root: &Path) -> Result<SupervisorManagerOperability> {
        let mut state = self.state.lock().unwrap();
        state.manager_probes += 1;
        if self.manager_probe_error {
            return Err(anyhow!("fake manager identity probe failed"));
        }
        if state.manager_unavailable {
            Ok(SupervisorManagerOperability::Unavailable {
                reason: "fake native manager unavailable".to_owned(),
            })
        } else {
            Ok(SupervisorManagerOperability::Operational)
        }
    }

    fn prepare_mutation(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.mutation_preparations += 1;
        if self.mutation_preparation_error {
            Err(anyhow!("fake daemon ownership preparation failed"))
        } else {
            Ok(())
        }
    }

    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        Ok(Some(self.artifact_path_override.clone().unwrap_or_else(
            || data_root.join("fake-native-registration"),
        )))
    }

    fn install(
        &self,
        data_root: &Path,
        _executable: &Path,
        _environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf> {
        {
            let mut state = self.state.lock().unwrap();
            state.installs += 1;
        }
        if self.delay_install {
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut state = self.state.lock().unwrap();
        if self.fail_install_without_registration {
            return Err(anyhow!("fake installer failed before registration"));
        }
        state.registered = true;
        state.live_owner = Some(4_242);
        if self.manager_unavailable_after_install {
            state.manager_unavailable = true;
        }
        if self.fail_install_after_registration {
            return Err(anyhow!(
                "fake installer failed after publishing valid registration"
            ));
        }
        Ok(data_root.join("fake-native-registration"))
    }

    fn disable(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
        let mut state = self.state.lock().unwrap();
        state.disables += 1;
        if self.fail_disable {
            if self.manager_unavailable_on_disable_failure {
                state.manager_unavailable = true;
            }
            return Err(anyhow!("fake native disable failed"));
        }
        state.registered = false;
        state.live_owner = None;
        Ok(None)
    }

    fn verify_registration(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.registration_probes += 1;
        state
            .registered
            .then_some(())
            .ok_or_else(|| anyhow!("fake native registration is absent"))
    }

    fn verify_live_owner(&self, _data_root: &Path, _executable: &Path) -> Result<u32> {
        self.state
            .lock()
            .unwrap()
            .live_owner
            .ok_or_else(|| anyhow!("fake native manager has no owner"))
    }

    fn start(&self, _data_root: &Path) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.starts += 1;
        if self.fail_start {
            return Err(anyhow!("fake native manager refused to start daemon"));
        }
        state.start_observed_released_fence = state.upgrade_fence_released;
        state.live_owner = Some(4_242);
        Ok(())
    }
}

#[test]
fn manager_control_environment_is_exact_and_rejects_release_authority() -> Result<()> {
    let exact = normalized_supervisor_manager_environment(BTreeMap::from([
        (OsString::from("PATH"), OsString::from("/manager/bin")),
        (OsString::from("HOME"), OsString::from("/manager/home")),
    ]))?;
    let command = supervisor_command("manager", &exact);
    let applied = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_os_string(),
                value
                    .expect("manager environment values are explicit")
                    .to_os_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(&applied, exact.values());

    let error = normalized_supervisor_manager_environment(BTreeMap::from([(
        OsString::from("CTX_RELEASE_BASE_URL"),
        OsString::from("https://attacker.invalid"),
    )]))
    .expect_err("release authority must be rejected before mechanics");
    assert!(error.to_string().contains("release authority variable"));
    Ok(())
}

#[test]
fn managed_supervisor_input_freezes_daemon_and_manager_environments() -> Result<()> {
    struct RestoreEnvironment {
        pro_channel: Option<OsString>,
        home: Option<OsString>,
    }
    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            match self.pro_channel.take() {
                Some(value) => env::set_var("CTX_PRO_CHANNEL", value),
                None => env::remove_var("CTX_PRO_CHANNEL"),
            }
            match self.home.take() {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }
    }

    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreEnvironment {
        pro_channel: env::var_os("CTX_PRO_CHANNEL"),
        home: env::var_os("HOME"),
    };
    env::set_var("CTX_PRO_CHANNEL", "stable");
    env::set_var("HOME", "/manager-before-normalization");
    let input = ManagedSupervisorInput::new(&TestHost, Path::new("/data"), Path::new("/bin/ctx"))?;
    env::set_var("CTX_PRO_CHANNEL", "staging");
    env::set_var("HOME", "/manager-after-normalization");

    assert!(input
        .daemon_environment
        .values
        .contains(&("CTX_PRO_CHANNEL".to_owned(), "stable".to_owned())));
    assert!(!input
        .daemon_environment
        .values
        .contains(&("CTX_PRO_CHANNEL".to_owned(), "staging".to_owned())));
    assert_eq!(
        input.manager_environment.get("HOME"),
        Some(OsStr::new("/manager-before-normalization"))
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_unit_is_persistent_and_restart_on_failure() {
    let unit = linux_systemd_unit(
        Path::new("/home/user/.local/bin/ctx"),
        Path::new("/home/user/.local/share/ctx"),
    )
    .unwrap();
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(unit.contains("ExecStart=/usr/bin/env -i "));
    assert!(!unit.contains("CTX_RELEASE_"));
    assert!(!unit.contains("idle-exit-seconds"));
    assert!(!unit.contains("loop-interval-seconds"));
}

#[test]
fn systemd_registration_requires_a_nonzero_live_main_pid() {
    assert_eq!(systemd_main_pid(b"4242\n").unwrap(), 4242);
    assert!(systemd_main_pid(b"0\n").is_err());
    assert!(systemd_main_pid(b"\n").is_err());
}

#[test]
fn launch_agent_plist_is_persistent_sanitized_and_gui_registration_is_identity_bearing() {
    let plist = launch_agent_plist(
        Path::new("/Users/test/Library/Application Support/ctx/ctx"),
        Path::new("/Users/test/Library/Application Support/ctx/data"),
    )
    .unwrap();
    assert!(plist.contains("<key>Label</key><string>rs.ctx.daemon</string>"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<string>/usr/bin/env</string><string>-i</string>"));
    assert!(!plist.contains("CTX_RELEASE_"));
    assert!(!plist.contains("idle-exit-seconds"));
    assert_eq!(
        launchctl_print_pid("state = running\n\tpid = 73\n"),
        Some(73)
    );
    assert_eq!(launchctl_print_pid("state = waiting\n"), None);
}

#[test]
fn windows_task_contract_is_current_user_restartable_and_spawns_with_a_clear_environment() {
    let script = windows_sanitized_daemon_script(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
    )
    .unwrap();
    assert!(script.contains("EnvironmentVariables.Clear()"));
    assert!(script.contains("UseShellExecute=$false"));
    assert!(script.contains("while($true)"));
    assert!(script.contains("if($code -eq 0){exit 0}"));
    assert!(script.contains("Start-Sleep -Seconds $delay"));
    assert!(script.contains("$delay=[Math]::Min($delay*2,60)"));
    assert!(script.contains("finally{if($null -ne $c){$c.Dispose()}}"));
    assert!(!script.contains("exit $c.ExitCode"));
    assert!(!script.contains("if($c.ExitCode -eq 0){exit 0};exit 1"));
    assert!(!script.contains("CTX_RELEASE_"));
    assert!(!script.contains("idle-exit-seconds"));

    let xml = windows_task_xml(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .unwrap();
    let xml_bytes = windows_task_xml_bytes(&xml);
    assert!(xml_bytes.starts_with(&[0xff, 0xfe]));
    assert_eq!(decode_supervisor_text(&xml_bytes), xml);
    let registration_matches = |candidate: &str| {
        windows_task_registration_matches(
            candidate,
            Path::new(r"C:\Program Files\ctx\ctx.exe"),
            Path::new(r"C:\Users\test\AppData\Local\ctx"),
            Path::new(r"C:\Windows"),
            "S-1-5-21-1000",
            r"\ctx-daemon-S-1-5-21-1000",
        )
        .unwrap()
    };
    assert!(registration_matches(&xml));
    assert!(xml.contains("<LogonTrigger>"));
    assert!(xml.contains("<UserId>S-1-5-21-1000</UserId>"));
    assert!(xml.contains("<RestartOnFailure>"));
    assert!(xml.contains("<Interval>PT1M</Interval>"));
    assert!(xml.contains("<Count>255</Count>"));
    assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    assert!(!registration_matches(
        "<Task><LogonType>InteractiveToken</LogonType></Task>"
    ));
    let no_logon_trigger = xml
        .replace("<LogonTrigger>", "<TimeTrigger>")
        .replace("</LogonTrigger>", "</TimeTrigger>");
    assert!(!registration_matches(&no_logon_trigger));
    assert!(!registration_matches(
        &xml.replace("<Enabled>true</Enabled>", "<Enabled>false</Enabled>")
    ));
    let normalized_defaults = xml
        .replace("<Enabled>true</Enabled>", "")
        .replace("<RunLevel>LeastPrivilege</RunLevel>", "");
    assert!(registration_matches(&normalized_defaults));
    assert!(windows_task_user_identity_matches(
        r"CTX-WIN11\ctxlab",
        "S-1-5-21-1000",
        Some("S-1-5-21-1000"),
    ));
    assert!(!windows_task_user_identity_matches(
        r"CTX-WIN11\ctxlab",
        "S-1-5-21-1000",
        Some("S-1-5-21-2000"),
    ));
    let disabled_trigger_with_task_enabled = xml
        .replace("<Enabled>true</Enabled>", "<Enabled>false</Enabled>")
        .replace("<Settings>", "<Settings><Enabled>true</Enabled>");
    assert!(!registration_matches(&disabled_trigger_with_task_enabled));
    assert!(!registration_matches(
        &xml.replace("<Settings>", "<Settings><Enabled>false</Enabled>")
    ));
    assert!(!registration_matches(&xml.replace(
        "</Triggers>",
        "<TimeTrigger><StartBoundary>2026-01-01T00:00:00</StartBoundary></TimeTrigger></Triggers>",
    )));
    assert!(!registration_matches(
        &xml.replace("</Triggers>", "<EventTrigger/></Triggers>")
    ));
    assert!(!registration_matches(
        &xml.replace("</Actions>", "<ComHandler/></Actions>")
    ));
    assert!(!registration_matches(&xml.replace(
        WINDOWS_TASK_XML_NAMESPACE,
        "https://example.invalid/not-task-scheduler",
    )));
    assert!(!registration_matches(&xml.replace(
        "<Enabled>true</Enabled>",
        "<Enabled xmlns=\"https://example.invalid/not-task-scheduler\">true</Enabled>",
    )));
    assert!(!registration_matches(
        &xml.replace("<Enabled>true</Enabled>", "<Enabled></Enabled>")
    ));
    assert!(!registration_matches(&xml.replace(
        "<RunLevel>LeastPrivilege</RunLevel>",
        "<RunLevel></RunLevel>",
    )));
    assert!(!registration_matches(&xml.replace(
        "<Count>255</Count>",
        "<Count>255</Count><Count>255</Count>"
    )));
    assert!(!registration_matches(
        &xml.replace("<Count>255</Count>", "<Count/>")
    ));
    let truncated = xml.strip_suffix("</Task>\n").unwrap();
    assert!(!registration_matches(truncated));
    assert!(!registration_matches(&format!("{xml}<Other/>")));
    assert!(!registration_matches(&format!("{xml}unexpected")));
    assert!(!registration_matches(&xml.replace(
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        "<ExecutionTimeLimit>PT1H</ExecutionTimeLimit>",
    )));
    assert_eq!(
        windows_task_name("S-1-5-21-1000"),
        r"\ctx-daemon-S-1-5-21-1000"
    );
    let state_script = windows_task_state_script(r"\ctx-daemon-S-1-5-21-1000");
    assert!(state_script.contains("-TaskPath '\\'"));
    assert!(state_script.contains("-TaskName 'ctx-daemon-S-1-5-21-1000'"));
    assert_eq!(parse_windows_task_state(b"4\r\n"), Some(4));
    assert_ne!(parse_windows_task_state(b"3\r\n"), Some(4));
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn supervisor_artifact_atomic_write_replaces_existing_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("supervisor-artifact");
    write_atomic_file(&path, b"first").unwrap();
    write_atomic_file(&path, b"second").unwrap();
    assert_eq!(fs::read(path).unwrap(), b"second");
}

#[cfg(windows)]
#[test]
fn windows_task_xml_registers_with_task_scheduler() -> Result<()> {
    struct TaskCleanup {
        task_name: String,
        powershell: PathBuf,
    }

    impl Drop for TaskCleanup {
        fn drop(&mut self) {
            let _ = Command::new("schtasks")
                .args(["/End", "/TN"])
                .arg(&self.task_name)
                .output();
            let state_script = windows_task_state_script(&self.task_name);
            for _ in 0..50 {
                let Ok(output) = Command::new(&self.powershell)
                    .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
                    .arg(&state_script)
                    .output()
                else {
                    break;
                };
                if !output.status.success() || parse_windows_task_state(&output.stdout) != Some(4) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = Command::new("schtasks")
                .args(["/Delete", "/TN"])
                .arg(&self.task_name)
                .arg("/F")
                .output();
        }
    }

    let manager_environment = supervisor_manager_environment(&TestHost)?;
    let sid = current_windows_user_sid(&manager_environment)?;
    let task_name = format!(r"\ctx-test-daemon-xml-{}", std::process::id());
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("windows-task.xml");
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let cleanup = TaskCleanup {
        task_name: task_name.clone(),
        powershell: powershell.clone(),
    };
    let executable = Path::new(&system_root).join("System32").join("where.exe");
    let stale_xml = windows_task_xml(
        &executable,
        &temp.path().join("stale-data"),
        Path::new(&system_root),
        &sid,
        &task_name,
    )?;
    write_atomic_file(&path, &windows_task_xml_bytes(&stale_xml))?;
    let create_stale = Command::new("schtasks")
        .args(["/Create", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .arg(&path)
        .arg("/F")
        .output()?;
    if !create_stale.status.success() {
        return Err(anyhow!(
            "schtasks /Create rejected stale generated XML: {}{}",
            String::from_utf8_lossy(&create_stale.stdout),
            String::from_utf8_lossy(&create_stale.stderr)
        ));
    }

    let xml = windows_task_xml(
        &executable,
        &temp.path().join("data"),
        Path::new(&system_root),
        &sid,
        &task_name,
    )?;
    write_atomic_file(&path, &windows_task_xml_bytes(&xml))?;

    let create = Command::new("schtasks")
        .args(["/Create", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .arg(&path)
        .arg("/F")
        .output()?;
    if !create.status.success() {
        return Err(anyhow!(
            "schtasks /Create rejected generated XML: {}{}",
            String::from_utf8_lossy(&create.stdout),
            String::from_utf8_lossy(&create.stderr)
        ));
    }
    let query = Command::new("schtasks")
        .args(["/Query", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .output()?;
    assert!(
        query.status.success(),
        "schtasks /Query failed: {}{}",
        String::from_utf8_lossy(&query.stdout),
        String::from_utf8_lossy(&query.stderr)
    );
    assert!(windows_task_registration_matches(
        &decode_supervisor_text(&query.stdout),
        &executable,
        &temp.path().join("data"),
        Path::new(&system_root),
        &sid,
        &task_name,
    )?);

    let probe = temp.path().join("restart-probe.ps1");
    let counter = temp.path().join("restart-count.txt");
    let marker = temp.path().join("restart-recovered.txt");
    let probe_script = format!(
        "$ErrorActionPreference='Stop';$countPath='{}';$markerPath='{}';$count=0;if(Test-Path -LiteralPath $countPath){{$count=[int](Get-Content -Raw -LiteralPath $countPath)}};$count++;[IO.File]::WriteAllText($countPath,[string]$count);if($count -eq 1){{exit 23}};[IO.File]::WriteAllText($markerPath,'recovered');exit 0",
        powershell_single_quote(validated_supervisor_artifact_path(
            "Windows restart test counter",
            &counter,
        )?),
        powershell_single_quote(validated_supervisor_artifact_path(
            "Windows restart test marker",
            &marker,
        )?),
    );
    let mut probe_bytes = vec![0xff, 0xfe];
    probe_bytes.extend(probe_script.encode_utf16().flat_map(u16::to_le_bytes));
    fs::write(&probe, probe_bytes)?;
    let probe_arguments = vec![
        "-NoLogo".to_owned(),
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-File".to_owned(),
        validated_supervisor_artifact_path("Windows restart test probe", &probe)?.to_owned(),
    ];
    let action_script = windows_sanitized_process_supervisor_script(
        &powershell,
        &probe_arguments,
        &supervisor_environment_snapshot(&TestHost)?,
    )?;
    let probe_xml =
        windows_task_xml_with_script(Path::new(&system_root), &sid, &task_name, &action_script)?;
    write_atomic_file(&path, &windows_task_xml_bytes(&probe_xml))?;
    let replace = Command::new("schtasks")
        .args(["/Create", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .arg(&path)
        .arg("/F")
        .output()?;
    if !replace.status.success() {
        return Err(anyhow!(
            "schtasks /Create rejected restart-probe XML: {}{}",
            String::from_utf8_lossy(&replace.stdout),
            String::from_utf8_lossy(&replace.stderr)
        ));
    }

    let run = Command::new("schtasks")
        .args(["/Run", "/TN"])
        .arg(&task_name)
        .output()?;
    assert!(
        run.status.success(),
        "schtasks /Run failed: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let recovery_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < recovery_deadline && !marker.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        marker.exists(),
        "scheduled action did not relaunch its failing child"
    );
    assert_eq!(
        fs::read_to_string(&counter)?.trim(),
        "2",
        "scheduled action did not recover on exactly the second child launch"
    );

    let task = task_name.trim_start_matches('\\');
    let result_script = format!(
        "$i=Get-ScheduledTaskInfo -TaskPath '\\' -TaskName '{}' -ErrorAction Stop;[Console]::Out.Write([uint32]$i.LastTaskResult)",
        task,
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_result = None;
    while Instant::now() < deadline {
        let result = Command::new(&powershell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(&result_script)
            .output()?;
        if result.status.success() {
            last_result = decode_supervisor_text(&result.stdout).trim().parse().ok();
            if last_result == Some(0) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        last_result,
        Some(0),
        "scheduled action did not finish successfully after recovering its child"
    );

    drop(cleanup);
    let absent = Command::new("schtasks")
        .args(["/Query", "/TN"])
        .arg(&task_name)
        .output()?;
    assert!(!absent.status.success());
    Ok(())
}

#[test]
fn native_supervisor_artifacts_exclude_authority_and_fail_closed_on_controls() -> Result<()> {
    let forbidden = [
        "CTX_PRO_HELPER",
        "CTX_PRO_STAGING_ACCESS_CLIENT_ID",
        "CTX_PRO_STAGING_ACCESS_CLIENT_SECRET",
        "CTX_PRO_QUALIFICATION_HELPER_PATH",
        "CTX_PRO_QUALIFICATION_HELPER_SHA256",
        "CTX_PRO_QUALIFICATION_HELPER_CHANNEL",
        "CTX_PRO_API_URL",
        "CTX_SEMANTIC_MODEL_ONNX",
        "CTX_RELEASE_CONFIGURED_AUTHORITY",
        "CTX_RELEASE_METADATA_URL",
        "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
        "CTX_RELEASE_METADATA_SIGNATURE_URL",
        "CTX_RELEASE_PUBLIC_KEY",
        "CTX_RELEASE_SIGNATURE",
        "CTX_RELEASE_VERSION",
        "GITHUB_TOKEN",
    ];
    if env::var(SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE).as_deref() != Ok("final") {
        let mut child = Command::new(env::current_exe()?);
        child
            .args(["--exact", SUPERVISOR_ENV_ARTIFACT_PROBE_TEST, "--nocapture"])
            .env(SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE, "final")
            .env("CTX_PRO_CHANNEL", "staging");
        for name in forbidden {
            child.env(name, format!("secret-value-for-{name}"));
        }
        assert!(child.status()?.success());
        return Ok(());
    }

    let executable = Path::new("/opt/ctx/bin/ctx");
    let data_root = Path::new("/tmp/ctx-native-supervisor-environment");
    let systemd = linux_systemd_unit(executable, data_root)?;
    let launchd = launch_agent_plist(executable, data_root)?;
    let windows = windows_sanitized_daemon_script(executable, data_root)?;
    for artifact in [&systemd, &launchd, &windows] {
        assert!(
            artifact.contains("CTX_PRO_CHANNEL=staging")
                || artifact.contains("['CTX_PRO_CHANNEL']='staging'"),
            "staging Pro channel missing from {artifact}"
        );
    }
    for name in forbidden {
        let value = format!("secret-value-for-{name}");
        for artifact in [&systemd, &launchd, &windows] {
            assert!(!artifact.contains(name), "{name} leaked into {artifact}");
            assert!(
                !artifact.contains(&value),
                "{name} value leaked into {artifact}"
            );
        }
    }

    env::set_var("CODEX_HOME", "line\nbreak");
    assert!(linux_systemd_unit(executable, data_root).is_err());
    assert!(launch_agent_plist(executable, data_root).is_err());
    assert!(windows_sanitized_daemon_script(executable, data_root).is_err());
    assert!(windows_task_xml(
        executable,
        data_root,
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .is_err());
    let hostile_root = Path::new("/tmp/ctx\ninjected-directive");
    assert!(linux_systemd_unit(executable, hostile_root).is_err());
    assert!(launch_agent_plist(executable, hostile_root).is_err());
    assert!(windows_task_xml(
        executable,
        hostile_root,
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .is_err());
    Ok(())
}

#[test]
fn windows_task_status_decoder_handles_task_scheduler_utf16_xml() {
    let source =
        r#"<Task><RegistrationInfo><URI>\ctx-daemon-S-1-5-21-1000</URI></RegistrationInfo></Task>"#;
    let mut encoded = vec![0xff, 0xfe];
    encoded.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(decode_supervisor_text(&encoded), source);
}

#[test]
fn windows_command_line_quoting_preserves_spaces_quotes_and_trailing_separators() {
    assert_eq!(windows_command_line_quote("plain"), "plain");
    assert_eq!(windows_command_line_quote("two words"), "\"two words\"");
    assert_eq!(windows_command_line_quote(r#"C:\a b\"#), r#""C:\a b\\""#,);
}

#[test]
fn freebsd_limitation_names_the_missing_product_authority_without_claiming_support() {
    let limitation = freebsd_supervisor_authority_blocker();
    assert!(limitation.contains("no standard current-user service manager"));
    assert!(limitation.contains("will not mutate the user's crontab"));
    assert!(limitation.contains("typed CLI self-healing"));
}

#[test]
fn concurrent_recovery_revalidates_registration_under_the_installation_lock() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = Arc::new(FakeSupervisorBackend {
        delay_install: true,
        fail_install_after_registration: true,
        ..FakeSupervisorBackend::default()
    });
    let barrier = Arc::new(Barrier::new(3));
    let callers = (0..2)
        .map(|_| {
            let data_root = temp.path().to_path_buf();
            let executable = executable.clone();
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ensure_native_supervisor_with(
                    &TestHost,
                    &ManagedSupervisorInput::new(&TestHost, &data_root, &executable)?,
                    backend.as_ref(),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for caller in callers {
        assert_eq!(
            caller
                .join()
                .expect("join concurrent supervisor recovery")?,
            DaemonSupervisorStart::Native
        );
    }
    let state = backend.state.lock().unwrap();
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 0);
    assert_eq!(state.starts, 0);
    assert!(state.registered);
    assert_eq!(state.live_owner, Some(4_242));
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "installed");
    assert_eq!(receipt["registration_verified"], true);
    assert_eq!(receipt["live_owner_verified"], true);
    assert_eq!(receipt["owner_pid"], 4_242);
    assert_eq!(receipt["environment_snapshot"]["schema_version"], 1);
    assert!(receipt["environment_snapshot"]["captured_at_ms"]
        .as_i64()
        .is_some());
    assert!(receipt["environment_snapshot"]["sha256"]
        .as_str()
        .is_some_and(|value| value.len() == 64));
    assert_eq!(receipt["environment_snapshot"]["values_exposed"], false);
    assert!(receipt["environment_snapshot"].get("values").is_none());
    Ok(())
}

#[test]
fn unavailable_manager_falls_back_before_native_mutation_under_the_installation_lock() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    backend.state.lock().unwrap().manager_unavailable = true;

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 2);
    assert_eq!(state.mutation_preparations, 0);
    assert_eq!(state.registration_probes, 0);
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    assert_eq!(state.starts, 0);
    drop(state);
    assert!(ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor-installation.lock")
        .exists());

    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "manager_unavailable");
    assert_eq!(report["autostart_supported"], false);
    assert_eq!(report["restart_supported"], false);
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert!(report["limitation"]
        .as_str()
        .is_some_and(|value| value.contains("continuous refresh is unavailable")));
    Ok(())
}

#[test]
fn unavailable_manager_receipt_waits_for_the_installation_lock() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().to_path_buf();
    let executable = data_root.join("ctx");
    let held_lock = SupervisorInstallationLock::acquire(&data_root)?;
    let backend = Arc::new(FakeSupervisorBackend::default());
    backend.state.lock().unwrap().manager_unavailable = true;

    let worker_backend = Arc::clone(&backend);
    let worker_root = data_root.clone();
    let worker_executable = executable.clone();
    let worker = std::thread::spawn(move || {
        ensure_native_supervisor_with(
            &TestHost,
            &ManagedSupervisorInput::new(&TestHost, &worker_root, &worker_executable)?,
            worker_backend.as_ref(),
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while backend.state.lock().unwrap().manager_probes == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "manager preflight did not reach the held installation lock"
        );
        std::thread::yield_now();
    }
    assert!(
        !ctx_daemon_runtime::daemon_root_path(&data_root)
            .join("supervisor.json")
            .exists(),
        "manager-unavailable receipt must not race ahead of the installation lock"
    );

    drop(held_lock);
    assert_eq!(
        worker.join().expect("join manager-unavailable setup")?,
        DaemonSupervisorStart::ManagerUnavailable
    );
    assert_eq!(
        stored_supervisor_report(&data_root)["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn unavailable_manager_artifact_inspection_errors_remain_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let blocked_parent = temp.path().join("artifact-parent-is-a-file");
    fs::write(&blocked_parent, b"not a directory")?;
    let backend = FakeSupervisorBackend {
        artifact_path_override: Some(blocked_parent.join("ctx.service")),
        ..FakeSupervisorBackend::default()
    };
    backend.state.lock().unwrap().manager_unavailable = true;

    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &temp.path().join("ctx"))?,
        &backend,
    )
    .expect_err("artifact metadata errors must not be treated as absence");
    assert!(
        error
            .to_string()
            .contains("inspect native supervisor artifact"),
        "{error:#}"
    );
    assert!(!ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor.json")
        .exists());
    Ok(())
}

#[test]
fn manager_loss_after_partial_registration_preserves_state_and_falls_back() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_install_after_registration: true,
        manager_unavailable_after_install: true,
        ..FakeSupervisorBackend::default()
    };

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert!(state.registered);
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 0);
    drop(state);

    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "manager_unavailable");
    assert!(report["limitation"]
        .as_str()
        .is_some_and(|value| value.contains("state was preserved")));
    Ok(())
}

#[test]
fn manager_loss_during_partial_cleanup_is_a_degraded_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_install_without_registration: true,
        fail_disable: true,
        manager_unavailable_on_disable_failure: true,
        ..FakeSupervisorBackend::default()
    };

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 1);
    assert!(state.manager_unavailable);
    drop(state);
    assert_eq!(
        stored_supervisor_report(temp.path())["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn manager_unavailable_upgrade_receipt_waits_for_lock_and_preserves_fence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().to_path_buf();
    let executable = data_root.join("ctx");
    let held_lock = SupervisorInstallationLock::acquire(&data_root)?;
    let backend = Arc::new(FakeSupervisorBackend::with_registration(Some(4_242)));
    backend.state.lock().unwrap().manager_unavailable = true;
    let fence_released = Arc::new(AtomicBool::new(false));

    let worker_backend = Arc::clone(&backend);
    let worker_root = data_root.clone();
    let worker_executable = executable.clone();
    let worker_released = Arc::clone(&fence_released);
    let worker = std::thread::spawn(move || {
        let mut fence = TestSupervisorUpgradeFence(Some(move || {
            worker_released.store(true, Ordering::SeqCst);
            Ok(())
        }));
        resume_daemon_supervisor_after_upgrade_with(
            &TestHost,
            &worker_root,
            &worker_executable,
            worker_backend.as_ref(),
            &mut fence,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while backend.state.lock().unwrap().manager_probes == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "upgrade manager preflight did not reach the held installation lock"
        );
        std::thread::yield_now();
    }
    assert!(!fence_released.load(Ordering::SeqCst));
    assert!(!ctx_daemon_runtime::daemon_root_path(&data_root)
        .join("supervisor.json")
        .exists());

    drop(held_lock);
    assert_eq!(
        worker.join().expect("join manager-unavailable upgrade")?,
        DaemonSupervisorUpgradeResume::ManagerUnavailable
    );
    assert!(!fence_released.load(Ordering::SeqCst));
    assert_eq!(
        stored_supervisor_report(&data_root)["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn operational_manager_cleanup_and_probe_integrity_failures_remain_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let cleanup_failure = FakeSupervisorBackend {
        fail_install_without_registration: true,
        fail_disable: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &cleanup_failure,
    )
    .expect_err("an operational manager cleanup failure must remain fatal");
    assert!(format!("{error:#}").contains("fake installer failed"));
    assert_eq!(cleanup_failure.state.lock().unwrap().disables, 1);

    let probe_failure = FakeSupervisorBackend {
        manager_probe_error: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &probe_failure,
    )
    .expect_err("manager identity/probe errors must remain fatal");
    assert!(error.to_string().contains("identity probe failed"));
    let state = probe_failure.state.lock().unwrap();
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    drop(state);

    let ownership_failure = FakeSupervisorBackend {
        mutation_preparation_error: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &ownership_failure,
    )
    .expect_err("daemon ownership preparation failures must remain fatal");
    assert!(error.to_string().contains("ownership preparation failed"));
    let state = ownership_failure.state.lock().unwrap();
    assert_eq!(state.mutation_preparations, 1);
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    Ok(())
}

#[test]
fn operational_manager_without_an_identity_verified_owner_remains_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_start: true,
        ..FakeSupervisorBackend::with_registration(None)
    };

    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )
    .expect_err("operational manager ownership failures must not degrade to fallback");
    assert!(
        format!("{error:#}").contains("identity-verified daemon ownership"),
        "{error:#}"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert_eq!(state.installs, 0);
    drop(state);
    assert_eq!(
        stored_supervisor_report(temp.path())["status"],
        "registered_not_running"
    );
    Ok(())
}

#[test]
fn unavailable_manager_prevents_native_disable_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.state.lock().unwrap().manager_unavailable = true;

    let error = disable_native_supervisor_candidate_with(
        temp.path(),
        Some(temp.path().join("ctx")),
        &backend,
    )
    .expect_err("disable must preserve native state while its manager is unavailable");
    assert!(
        error
            .to_string()
            .contains("no registration state was changed"),
        "{error:#}"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 1);
    assert_eq!(state.disables, 0);
    assert!(state.registered);
    Ok(())
}

#[test]
fn upgrade_handoff_releases_fence_before_native_manager_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(None);
    let mut fence = TestSupervisorUpgradeFence(Some(|| {
        backend.state.lock().unwrap().upgrade_fence_released = true;
        Ok(())
    }));
    let result = resume_daemon_supervisor_after_upgrade_with(
        &TestHost,
        temp.path(),
        &executable,
        &backend,
        &mut fence,
    )?;
    assert_eq!(result, DaemonSupervisorUpgradeResume::Native);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert!(state.start_observed_released_fence);
    assert_eq!(state.live_owner, Some(4_242));
    drop(state);
    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "installed");
    assert_eq!(report["owner_pid"], 4_242);
    Ok(())
}

#[test]
fn upgrade_handoff_keeps_fence_for_detached_fallback_without_native_registration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    let fence_released = AtomicBool::new(false);
    let mut fence = TestSupervisorUpgradeFence(Some(|| {
        fence_released.store(true, Ordering::SeqCst);
        Ok(())
    }));
    let result = resume_daemon_supervisor_after_upgrade_with(
        &TestHost,
        temp.path(),
        &executable,
        &backend,
        &mut fence,
    )?;
    assert_eq!(result, DaemonSupervisorUpgradeResume::Fallback);
    assert!(!fence_released.load(Ordering::SeqCst));
    assert_eq!(backend.state.lock().unwrap().starts, 0);
    Ok(())
}

#[test]
fn status_revalidates_registration_and_live_owner_instead_of_replaying_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;

    backend.state.lock().unwrap().live_owner = Some(7_331);
    let restarted = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(restarted["status"], "installed");
    assert_eq!(restarted["registration_verified"], true);
    assert_eq!(restarted["live_owner_verified"], true);
    assert_eq!(restarted["owner_pid"], 7_331);

    backend.state.lock().unwrap().live_owner = None;
    let stopped = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(stopped["status"], "registered_not_running");
    assert_eq!(stopped["registration_verified"], true);
    assert_eq!(stopped["live_owner_verified"], false);
    assert_eq!(stopped["owner_pid"], Value::Null);

    backend.state.lock().unwrap().registered = false;
    let stale = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(stale["status"], "stale_registration");
    assert_eq!(stale["registration_verified"], false);
    assert_eq!(stale["live_owner_verified"], false);
    Ok(())
}

#[test]
fn status_reports_manager_unavailability_without_registration_or_lock_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;
    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    receipt["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &receipt)?;
    backend.state.lock().unwrap().manager_unavailable = true;

    let report = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(report["status"], "manager_unavailable");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert_eq!(report["owner_pid"], Value::Null);
    assert_eq!(report["autostart_supported"], false);
    assert_eq!(report["restart_supported"], false);
    assert_eq!(
        report["environment_snapshot"]["restart_required"], false,
        "manager unavailability must remain the actionable persistence limitation"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 1);
    assert_eq!(state.registration_probes, 0);
    drop(state);
    assert!(!ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor-installation.lock")
        .exists());
    Ok(())
}

#[test]
fn status_invalidates_healthy_receipt_when_current_launch_environment_is_unreadable() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    write_supervisor_receipt(
        temp.path(),
        &SupervisorReceipt {
            kind: native_supervisor_kind().to_owned(),
            status: "installed",
            autostart_supported: true,
            restart_supported: true,
            registration_verified: true,
            live_owner_verified: true,
            owner_pid: Some(4_242),
            artifact_path: Some(temp.path().join("native")),
            executable_path: Some(temp.path().join("ctx")),
            limitation: None,
            last_error: None,
        },
    )?;
    let report = daemon_supervisor_report_with_normalized_environment(
        &TestHost,
        temp.path(),
        Err(anyhow!("CTX_PRO_CHANNEL is not Unicode")),
    );
    assert_eq!(report["status"], "environment_invalid");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert_eq!(report["owner_pid"], Value::Null);
    assert!(report["revalidation_error"]
        .as_str()
        .is_some_and(|error| error.contains("not trusted")));
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_control_context_accepts_nonunicode_manager_values_without_launch_snapshot() -> Result<()>
{
    use std::os::unix::ffi::OsStringExt as _;

    let manager_environment = normalized_supervisor_manager_environment(BTreeMap::from([(
        OsString::from("HOME"),
        OsString::from_vec(vec![b'/', 0xff]),
    )]))?;
    let backend = PlatformNativeSupervisor::new(
        &TestHost,
        Path::new("/tmp/ctx-control-test"),
        None,
        &manager_environment,
    )?;
    assert!(backend.launch_environment().is_err());
    // Removal/control mechanics consume only the manager context. This keeps
    // uninstall available when the launch-only environment cannot be Unicode.
    assert!(backend
        .artifact_path(Path::new("/tmp/ctx-control-test"))?
        .is_some());
    Ok(())
}

#[test]
fn status_preserves_installed_environment_hash_and_flags_current_mismatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;
    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let mut installed: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    installed["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    installed["environment_snapshot"]["captured_at_ms"] = json!(1234);
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &installed)?;

    let report = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(
        report["environment_snapshot"]["sha256"],
        "0".repeat(64),
        "status must retain the installed snapshot hash"
    );
    assert_eq!(report["environment_snapshot"]["captured_at_ms"], 1234);
    assert_eq!(report["environment_snapshot"]["restart_required"], true);
    assert_ne!(
        report["environment_snapshot"]["current_sha256"],
        report["environment_snapshot"]["sha256"]
    );
    assert_eq!(report["environment_snapshot"]["values_exposed"], false);
    Ok(())
}

#[test]
fn supervisor_report_states_forced_termination_identity_limitations() {
    let temp = tempfile::tempdir().unwrap();
    let report = daemon_supervisor_report(&TestHost, temp.path());
    if cfg!(target_os = "linux") {
        assert_eq!(
            report["forced_termination_identity"]["strategy"],
            "pidfd_when_available"
        );
        assert!(report["forced_termination_identity"]["limitation"]
            .as_str()
            .is_some_and(|value| value.contains("PID reuse")));
    } else if cfg!(unix) {
        assert_eq!(
            report["forced_termination_identity"]["strategy"],
            "reverified_pid"
        );
        assert!(report["forced_termination_identity"]["limitation"]
            .as_str()
            .is_some_and(|value| value.contains("cannot eliminate")));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn supervisor_live_ownership_requires_exact_manager_pid_and_executable() {
    let temp = tempfile::tempdir().unwrap();
    let _lock = ctx_daemon_runtime::DaemonLock::acquire(temp.path())
        .unwrap()
        .expect("daemon lock");
    let executable = env::current_exe().unwrap();
    assert_eq!(
        verify_daemon_owner_identity(temp.path(), &executable, Some(std::process::id())).unwrap(),
        std::process::id()
    );
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &executable,
        Some(std::process::id().saturating_add(1)),
    )
    .is_err());
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &temp.path().join("not-the-owner"),
        Some(std::process::id()),
    )
    .is_err());
}

#[test]
fn fallback_disable_status_is_retry_safe_without_claiming_registration() {
    let temp = tempfile::tempdir().unwrap();
    write_supervisor_receipt(
        temp.path(),
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
            limitation: Some("test limitation".to_owned()),
            last_error: None,
        },
    )
    .unwrap();
    disable_daemon_supervisor(&TestHost, temp.path()).unwrap();
    disable_daemon_supervisor(&TestHost, temp.path()).unwrap();
    let status = daemon_supervisor_report(&TestHost, temp.path());
    assert_eq!(status["status"], "disabled");
    assert_eq!(status["registration_verified"], false);
    assert_eq!(status["live_owner_verified"], false);
    assert_eq!(status["autostart_supported"], false);
    assert_eq!(status["restart_supported"], false);
}

#[test]
fn native_disable_attempts_surviving_registration_without_artifact_or_launch_probe() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));

    disable_native_supervisor_candidate_with(temp.path(), Some(temp.path().join("ctx")), &backend)?;

    let state = backend.state.lock().unwrap();
    assert_eq!(state.disables, 1);
    assert_eq!(state.registration_probes, 0);
    assert!(!state.registered);
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "disabled");
    assert_eq!(receipt["registration_verified"], false);
    Ok(())
}

#[test]
fn native_disable_failure_does_not_claim_an_unavailable_launch_probe_is_healthy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.fail_disable = true;

    assert!(disable_native_supervisor_candidate_with(
        temp.path(),
        Some(temp.path().join("ctx")),
        &backend,
    )
    .is_err());

    let state = backend.state.lock().unwrap();
    assert_eq!(state.disables, 1);
    assert_eq!(state.registration_probes, 0);
    assert!(state.registered);
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "disable_failed");
    assert_eq!(receipt["registration_verified"], false);
    assert!(receipt["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("failed")));
    Ok(())
}

#[test]
fn canonical_supervisor_root_is_independent_of_ctx_data_root_override() {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    let canonical = ctx_history_core::managed_data_root().unwrap();
    let custom = canonical.with_file_name("ctx-custom-supervisor-test");
    env::set_var("CTX_DATA_ROOT", &custom);

    assert!(is_canonical_managed_data_root(&canonical).unwrap());
    assert!(!is_canonical_managed_data_root(&custom).unwrap());

    if let Some(previous) = previous {
        env::set_var("CTX_DATA_ROOT", previous);
    } else {
        env::remove_var("CTX_DATA_ROOT");
    }
}
