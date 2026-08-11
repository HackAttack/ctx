use std::fs;

use ctx_agent_integrations::mcp_config::{
    install_target, ConfigStatus, McpAgentArg, McpInstallRequest, McpPathContext, McpStatusRequest,
};

use super::*;
use crate::IntegrationResultFact;

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};

#[test]
fn current_target_is_not_rewritten() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = b"{\n  \"unrelated\": true,\n  \"mcpServers\": {\n    \"ctx\": {\"command\": \"ctx\", \"args\": [\"mcp\", \"serve\"]}\n  }\n}\n";
    fs::write(path, original).unwrap();

    let outcome = install(
        McpInstallRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );

    assert_eq!(outcome.telemetry.result, Some(IntegrationResultFact::Ok));
    assert!(outcome.receipt.results[0].already_installed);
    assert!(!outcome.receipt.results[0].modified);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn status_recovery_preserves_selection_scope_and_conflict_force() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::Codex.target(true, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "[mcp_servers.ctx]\ncommand = 'other'\nargs = []\n").unwrap();

    let outcome = status(
        McpStatusRequest {
            agents: vec![McpAgentArg::Codex],
            all_agents: false,
            project: true,
        },
        &context,
        PRODUCT,
    );

    assert_eq!(outcome.receipt.results[0].status, ConfigStatus::Conflict);
    assert_eq!(
        outcome.recovery_command.as_deref(),
        Some("ctx integrations install mcp --agent codex --project --force")
    );
    assert_eq!(outcome.telemetry.conflicting_targets, Some(1));
}

#[test]
fn unsupported_project_target_is_counted_without_path_or_error_telemetry() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let outcome = status(
        McpStatusRequest {
            agents: vec![McpAgentArg::GitHubCopilot],
            all_agents: false,
            project: true,
        },
        &context,
        PRODUCT,
    );

    assert_eq!(outcome.receipt.results[0].status, ConfigStatus::Unsupported);
    assert_eq!(outcome.telemetry.unsupported_targets, Some(1));
    assert_eq!(outcome.recovery_command, None);
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn update_preserves_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "{\"unrelated\":true}").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();

    let result = install_target(&target, false);

    assert!(result.success);
    assert!(result.modified);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}
