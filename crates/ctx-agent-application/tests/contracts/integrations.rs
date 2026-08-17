mod support;

use support::*;

#[test]
fn slash_commands_install_opencode_global_and_is_idempotent() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-config");

    let first = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "--color=always",
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--format=json",
    ]));
    assert_eq!(first["integration"], "slash-commands");
    assert_eq!(first["command"], "ctx");
    assert_eq!(first["results"][0]["agent"], "opencode");
    assert_eq!(first["results"][0]["previous_status"], "missing");
    assert_eq!(first["results"][0]["status"], "current");
    assert_eq!(first["results"][0]["already_installed"], false);

    let command_path = xdg.join("opencode").join("commands").join("ctx.md");
    assert!(command_path.exists());
    assert!(fs::read_to_string(&command_path)
        .unwrap()
        .contains("$ARGUMENTS"));
    assert!(command_path
        .parent()
        .unwrap()
        .join(".ctx-slash-commands.json")
        .exists());

    let second = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--format=json",
    ]));
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);
}

#[test]
fn slash_commands_install_migrates_managed_ctx_history_to_ctx() {
    let temp = tempdir();
    let project = temp.path().join("project");
    let command_dir = project.join(".gemini").join("commands");
    let legacy_path = command_dir.join("ctx-history.toml");
    let command_path = command_dir.join("ctx.toml");
    let legacy_body = "prompt = 'managed legacy command'\n";
    fs::create_dir_all(&command_dir).unwrap();
    fs::write(&legacy_path, legacy_body).unwrap();
    fs::write(command_dir.join("keep.txt"), "keep").unwrap();
    fs::write(
        command_dir.join(".ctx-slash-commands.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "command_name": "ctx-history",
            "files": {
                "ctx-history.toml": "sha256:8e6ef57e9d2ba609496d3ac98016385cb9e9613d65a93430c5d7b7453accecfb"
            },
            "ctx_cli_version": "0.9.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--format=json",
    ]);
    let migrated = json_output(&mut command);
    assert_eq!(migrated["results"][0]["previous_status"], "stale");
    assert_eq!(migrated["results"][0]["status"], "current");
    assert_eq!(migrated["results"][0]["migrated"], true);
    assert_eq!(migrated["results"][0]["legacy_path"], json!(legacy_path));
    assert!(command_path.is_file());
    assert!(!legacy_path.exists());
    assert_eq!(
        fs::read_to_string(command_dir.join("keep.txt")).unwrap(),
        "keep"
    );

    let mut second = ctx(&temp);
    second.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--format=json",
    ]);
    let second = json_output(&mut second);
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["migrated"], false);
}

#[test]
fn slash_commands_install_codex_is_skill_only_without_deprecated_prompts() {
    let temp = tempdir();

    let output = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(output["results"][0]["agent"], "codex");
    assert_eq!(output["results"][0]["status"], "skill_only");
    assert!(output["results"][0]["note"]
        .as_str()
        .unwrap()
        .contains("ctx integrations install skills --agent codex"));
    assert!(!temp.path().join(".codex").join("prompts").exists());
}

#[test]
fn slash_commands_install_gemini_project_writes_toml() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--format=json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "gemini-cli");
    assert_eq!(
        output["results"][0]["path"],
        json!(project.join(".gemini/commands/ctx.toml"))
    );

    let command_path = project.join(".gemini").join("commands").join("ctx.toml");
    let body = fs::read_to_string(command_path).unwrap();
    assert!(body.contains("description ="));
    assert!(body.contains("prompt = '''"));
    assert!(body.contains("{{args}}"));
}

#[test]
fn slash_commands_install_qwen_project_writes_markdown() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "qwen-code",
        "--project",
        "--format=json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "qwen-code");

    let command_path = project.join(".qwen").join("commands").join("ctx.md");
    let body = fs::read_to_string(command_path).unwrap();
    assert!(body.contains("---\ndescription:"));
    assert!(body.contains("{{args}}"));
}
