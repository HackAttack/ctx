use anyhow::{anyhow, Result};
use ctx_agent_integrations::mcp_config::{
    execute_install, execute_status, McpInstallRequest, McpInstallResult, McpStatusRequest,
    McpStatusResult,
};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, IntegrationResult, IntegrationTelemetry},
    ui::{
        diagnostic, empty_state, fields, hint, outcome, section, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState, RenderContext,
        Ui,
    },
};

use super::{
    format::{self, ConfigStatus},
    registry::McpTarget,
    McpAgentArg, McpInstallArgs, McpPathContext, McpStatusArgs, SERVER_NAME,
};

pub(super) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let receipt = execute_install(
        McpInstallRequest {
            agents: args.agent.clone(),
            all_agents: args.all_agents,
            project: args.project,
            force: args.force,
        },
        context,
    );
    telemetry.resolved_agents = Some(count_bucket(receipt.selected_agents as u64));
    telemetry.result = Some(if receipt.failed == 0 {
        IntegrationResult::Ok
    } else {
        IntegrationResult::PartialError
    });
    telemetry.modified_targets = Some(count_bucket(receipt.modified as u64));
    if args.format.is_json() {
        let command = format::server_command();
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if receipt.project { "project" } else { "global" },
                "results": receipt.results.iter().map(mcp_install_result_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let document = render_install_results(ui.stdout_context(), &receipt.results);
        ui.write_stdout(&document)?;
        if let Some(diagnostics) = render_install_failures(ui.stderr_context(), &receipt.results) {
            ui.write_stderr(&diagnostics)?;
        }
    }
    if receipt.failed > 0 {
        if !args.format.is_json() {
            return Err(crate::dispatch::rendered_cli_error());
        }
        return Err(anyhow!(
            "failed to install MCP integration for {} target(s)",
            receipt.failed
        ));
    }
    Ok(())
}

pub(super) fn run_status(
    args: McpStatusArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let receipt = execute_status(
        McpStatusRequest {
            agents: args.agent.clone(),
            all_agents: args.all_agents,
            project: args.project,
        },
        context,
    );
    telemetry.resolved_agents = Some(count_bucket(receipt.selected_agents as u64));
    let results = &receipt.results;
    let status_count = |status| {
        results
            .iter()
            .filter(|result| result.status == status)
            .count()
    };
    telemetry.current_targets = Some(count_bucket(status_count(ConfigStatus::Current) as u64));
    telemetry.missing_targets = Some(count_bucket(status_count(ConfigStatus::Missing) as u64));
    telemetry.conflicting_targets = Some(count_bucket(status_count(ConfigStatus::Conflict) as u64));
    telemetry.invalid_targets = Some(count_bucket(status_count(ConfigStatus::Invalid) as u64));
    telemetry.unsupported_targets =
        Some(count_bucket(status_count(ConfigStatus::Unsupported) as u64));
    let current = status_count(ConfigStatus::Current);
    telemetry.result = Some(if current == results.len() {
        IntegrationResult::AllCurrent
    } else if current == 0 {
        IntegrationResult::NoneCurrent
    } else {
        IntegrationResult::PartiallyCurrent
    });
    if args.format.is_json() {
        let command = format::server_command();
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if receipt.request.project { "project" } else { "global" },
                "results": results.iter().map(mcp_status_result_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let recovery_command = status_install_command(&args, results);
        let document =
            render_status_results(ui.stdout_context(), results, recovery_command.as_deref());
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn dedupe_agents(agents: impl IntoIterator<Item = McpAgentArg>) -> Vec<McpAgentArg> {
    ctx_agent_integrations::mcp_config::dedupe_agents(agents)
}

fn mcp_install_result_json(result: &McpInstallResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.path,
        "detected": result.target.detected,
        "supported": result.target.unsupported_reason.is_none(),
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_installed": result.already_installed,
        "modified": result.modified,
        "error": result.error,
    })
}

fn mcp_status_result_json(result: &McpStatusResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.path,
        "detected": result.target.detected,
        "supported": result.target.unsupported_reason.is_none(),
        "status": result.status.as_str(),
        "error": result.error,
    })
}

fn render_install_results(context: &RenderContext, results: &[McpInstallResult]) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No MCP-capable coding agents detected",
                detail: "Select a coding agent explicitly or install every supported target.",
                action: Some(Action {
                    command: "ctx integrations install mcp --all-agents",
                }),
            },
        );
    }
    let all_current = results.iter().all(|result| result.already_installed);
    let all_success = results.iter().all(|result| result.success);
    let any_modified = results.iter().any(|result| result.modified);
    let title = if all_current {
        "ctx MCP integration is already installed"
    } else if all_success && any_modified {
        "ctx MCP integration installed"
    } else {
        "ctx MCP integration needs attention"
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if all_success {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title,
            detail: None,
        },
    );
    let command = format::server_command().render_for_host();
    document.push_blank();
    document.append(fields(context, &[Field::new("Server", &command)]));

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_installed {
                "current"
            } else if result.modified {
                "modified"
            } else if result.success {
                "ok"
            } else {
                "skipped"
            };
            (status, mcp_install_target_detail(result))
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    document
}

fn render_install_failures(
    context: &RenderContext,
    results: &[McpInstallResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} MCP configuration was not changed",
            result.target.agent.display_name()
        );
        let command = force_install_command(&result.target);
        if !document.is_empty() {
            document.push_blank();
        }
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: result.error.as_deref(),
                fields: &[],
                action: (result.status == ConfigStatus::Conflict)
                    .then_some(Action { command: &command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

fn force_install_command(target: &McpTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "ctx integrations install mcp --agent {}{project} --force",
        target.agent.id()
    )
}

fn render_status_results(
    context: &RenderContext,
    results: &[McpStatusResult],
    recovery_command: Option<&str>,
) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No MCP-capable coding agents detected",
                detail: "Select a coding agent explicitly or inspect every supported target.",
                action: Some(Action {
                    command: "ctx integrations status mcp --all-agents",
                }),
            },
        );
    }
    let all_current = results
        .iter()
        .all(|result| result.status == ConfigStatus::Current);
    let mut document = outcome(
        context,
        Outcome {
            state: if all_current {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: if all_current {
                "ctx MCP integration is current"
            } else {
                "ctx MCP integration needs attention"
            },
            detail: None,
        },
    );
    let command = format::server_command().render_for_host();
    document.push_blank();
    document.append(fields(context, &[Field::new("Server", &command)]));

    let rows = results
        .iter()
        .map(|result| (result.status.as_str(), mcp_status_target_detail(result)))
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    if let Some(command) = recovery_command {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Install or refresh MCP configuration for the affected targets.",
            },
            Some(Action { command }),
        ));
    }
    document
}

fn status_install_command(args: &McpStatusArgs, results: &[McpStatusResult]) -> Option<String> {
    let repairable = results
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                ConfigStatus::Missing | ConfigStatus::Conflict
            )
        })
        .collect::<Vec<_>>();
    if repairable.is_empty() {
        return None;
    }

    let mut tokens = ["ctx", "integrations", "install", "mcp"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let has_unrepairable = results.iter().any(|result| {
        matches!(
            result.status,
            ConfigStatus::Invalid | ConfigStatus::Unsupported
        )
    });
    if args.all_agents && !has_unrepairable {
        tokens.push("--all-agents".to_owned());
    } else if !args.agent.is_empty() && !has_unrepairable {
        for agent in dedupe_agents(args.agent.iter().copied()) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    } else {
        for agent in dedupe_agents(repairable.iter().map(|result| result.target.agent)) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    }
    if args.project {
        tokens.push("--project".to_owned());
    }
    if results
        .iter()
        .any(|result| result.status == ConfigStatus::Conflict)
    {
        tokens.push("--force".to_owned());
    }
    Some(tokens.join(" "))
}

fn mcp_install_target_detail(result: &McpInstallResult) -> String {
    let mut detail = result.target.agent.display_name().to_owned();
    if let Some(path) = &result.target.path {
        detail.push_str(" -> ");
        detail.push_str(&path.display().to_string());
    }
    detail
}

fn mcp_status_target_detail(result: &McpStatusResult) -> String {
    let mut detail = format!(
        "{} ({})",
        result.target.agent.display_name(),
        result.target.scope.as_str()
    );
    if let Some(path) = &result.target.path {
        detail.push_str(" -> ");
        detail.push_str(&path.display().to_string());
    }
    if let Some(error) = &result.error {
        detail.push_str(" - ");
        detail.push_str(error);
    }
    detail
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext, Token};
    use ctx_agent_integrations::mcp_config::{install_target, status_target};

    fn render_context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn semantic_command(document: &Document) -> String {
        document
            .lines()
            .iter()
            .flat_map(|line| line.spans())
            .filter(|span| span.token() == Token::Command)
            .map(|span| span.content())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn status_reports_unsupported_project_target() {
        let temp = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = McpAgentArg::GitHubCopilot.target(true, &context);
        let status = status_target(&target);
        assert_eq!(status.status, ConfigStatus::Unsupported);
        assert_eq!(
            status.error.as_deref(),
            Some("project-scoped MCP config is not documented for this agent")
        );
    }

    #[test]
    fn current_target_is_not_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let context = McpPathContext::for_tests(home, temp.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &context);
        let path = target.path.as_ref().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"{\n  \"unrelated\": true,\n  \"mcpServers\": {\n    \"ctx\": {\"command\": \"ctx\", \"args\": [\"mcp\", \"serve\"]}\n  }\n}\n";
        fs::write(path, original).unwrap();

        let result = install_target(&target, false);

        assert!(result.success);
        assert!(result.already_installed);
        assert!(!result.modified);
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn human_install_and_status_results_use_the_typed_ui() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let path_context = McpPathContext::for_tests(home, temp.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &path_context);
        let missing = status_target(&target);
        let installed = install_target(&target, false);
        let current = status_target(&target);

        for (document, expected) in [
            (
                render_install_results(&render_context(80, ColorMode::Never), &[installed]),
                "ctx MCP integration installed",
            ),
            (
                render_status_results(
                    &render_context(80, ColorMode::Never),
                    &[missing],
                    Some("ctx integrations install mcp --agent qwen-code"),
                ),
                "ctx MCP integration needs attention",
            ),
            (
                render_status_results(&render_context(80, ColorMode::Never), &[current], None),
                "ctx MCP integration is current",
            ),
        ] {
            let plain = document.render_plain();
            assert!(plain.contains(expected), "{plain}");
            assert!(plain.contains("Server"), "{plain}");
            assert!(plain.contains("Targets"), "{plain}");
        }

        let color = render_context(80, ColorMode::Always);
        let document = render_status_results(&color, &[status_target(&target)], None);
        let styled = document.render(&color);
        assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
        assert_eq!(strip_ansi(&styled), document.render_plain());
    }

    #[test]
    fn missing_mcp_status_offers_the_exact_selected_install_action() {
        let path_context = McpPathContext::for_tests("/home/test".into(), "/repo/test".into());
        let args = McpStatusArgs {
            agent: vec![McpAgentArg::Codex],
            all_agents: false,
            project: true,
            format: crate::output::JsonOutputFormat::Text,
        };
        let result = McpStatusResult {
            target: McpAgentArg::Codex.target(true, &path_context),
            status: ConfigStatus::Missing,
            error: None,
        };

        let command = status_install_command(&args, std::slice::from_ref(&result)).unwrap();
        assert_eq!(
            command,
            "ctx integrations install mcp --agent codex --project"
        );
        for width in [32, 48, 80, 120] {
            let context = render_context(width, ColorMode::Never);
            let document =
                render_status_results(&context, std::slice::from_ref(&result), Some(&command));
            assert_eq!(semantic_command(&document), command);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains("Install or refresh MCP configuration"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn mcp_conflict_names_the_selected_agent_in_the_force_action() {
        let path_context = McpPathContext::for_tests("/home/test".into(), "/repo/test".into());

        for (agent, project, expected_agent) in [
            (McpAgentArg::Cursor, false, "cursor"),
            (McpAgentArg::Codex, true, "codex"),
        ] {
            let result = McpInstallResult {
                target: agent.target(project, &path_context),
                success: false,
                previous_status: ConfigStatus::Conflict,
                status: ConfigStatus::Conflict,
                already_installed: false,
                modified: false,
                error: Some(
                    "existing ctx MCP server has different command or args; rerun with --force to overwrite"
                        .to_owned(),
                ),
            };
            let expected_project = if project { " --project" } else { "" };
            let expected = format!(
                "ctx integrations install mcp --agent {expected_agent}{expected_project} --force"
            );

            for width in [32, 48, 80, 120] {
                let plain_context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
                );
                let styled_context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Always),
                );
                let plain_document =
                    render_install_failures(&plain_context, std::slice::from_ref(&result)).unwrap();
                let styled_document =
                    render_install_failures(&styled_context, std::slice::from_ref(&result))
                        .unwrap();

                assert_eq!(semantic_command(&plain_document), expected);
                let normalized = plain_document
                    .render_plain()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(normalized.contains(&format!(
                    "{} MCP configuration was not changed",
                    agent.display_name()
                )));
                assert_eq!(
                    strip_ansi(&styled_document.render(&styled_context)),
                    plain_document.render_plain()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn update_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let context = McpPathContext::for_tests(home, temp.path().join("repo"));
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
}
