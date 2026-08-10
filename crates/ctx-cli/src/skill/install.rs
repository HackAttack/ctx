use anyhow::{anyhow, Result};
use ctx_agent_integrations::skill::{
    bundled_hash, execute_install, execute_status, InstallResult, SkillAgentSelection,
    SkillInstallRequest, SkillInstallStatus, SkillSelectionSource, SkillStatusRequest, SkillTarget,
    StatusResult,
};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, IntegrationResult, IntegrationTelemetry, TargetSelection},
    ui::{
        diagnostic, fields, hint, outcome, section, Action, Diagnostic, DiagnosticLevel, Document,
        Field, Hint, Outcome, OutcomeState, RenderContext, Ui,
    },
};

use super::{
    selection::{install_agent_selection, status_agent_selection},
    SkillInstallArgs, SkillStatusArgs, BUNDLED_SKILL_NAME,
};

pub(super) fn run_install(
    args: SkillInstallArgs,
    context: &super::paths::PathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let selection = install_agent_selection(&args, context)?;
    insert_selection_analytics(telemetry, &selection);
    let receipt = execute_install(
        SkillInstallRequest {
            selection,
            project: args.project,
            force: args.force,
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        context,
    )?;
    telemetry.result = Some(if receipt.fatal_failures == 0 {
        IntegrationResult::Ok
    } else {
        IntegrationResult::PartialError
    });
    telemetry.already_installed = Some(receipt.already_installed);
    telemetry.updated = Some(receipt.updated);
    telemetry.modified_targets = Some(count_bucket(receipt.modified_targets as u64));
    if args.format.is_json() {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": receipt.results.iter().map(install_result_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let document = render_install_results(ui.stdout_context(), &receipt.results);
        ui.write_stdout(&document)?;
        if let Some(diagnostics) = render_install_failures(ui.stderr_context(), &receipt.results) {
            ui.write_stderr(&diagnostics)?;
        }
    }
    if receipt.fatal_failures > 0 {
        if !args.format.is_json() {
            return Err(crate::dispatch::rendered_cli_error());
        }
        return Err(anyhow!(
            "failed to install skill for {} target(s)",
            receipt.fatal_failures
        ));
    }
    Ok(())
}

pub(super) fn run_status(
    args: SkillStatusArgs,
    context: &super::paths::PathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let selection = status_agent_selection(&args, context);
    insert_selection_analytics(telemetry, &selection);
    let receipt = execute_status(
        SkillStatusRequest {
            selection,
            project: args.project,
        },
        context,
    )?;
    telemetry.result = Some(if receipt.current_count == receipt.results.len() {
        IntegrationResult::AllCurrent
    } else if receipt.current_count == 0 {
        IntegrationResult::NoneCurrent
    } else {
        IntegrationResult::PartiallyCurrent
    });
    telemetry.current_targets = Some(count_bucket(receipt.current_count as u64));
    telemetry.missing_targets = Some(count_bucket(
        receipt
            .results
            .iter()
            .filter(|result| result.status == SkillInstallStatus::Missing)
            .count() as u64,
    ));
    telemetry.conflicting_targets = Some(count_bucket(
        receipt
            .results
            .iter()
            .filter(|result| result.status == SkillInstallStatus::Modified)
            .count() as u64,
    ));
    if args.format.is_json() {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": receipt.results.iter().map(status_result_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let recovery_command =
            status_install_command(&receipt.selection, args.project, &receipt.results);
        let document =
            render_status_results(ui.stdout_context(), &receipt.results, &recovery_command);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn insert_selection_analytics(
    telemetry: &mut IntegrationTelemetry,
    selection: &SkillAgentSelection,
) {
    telemetry.selection = Some(match selection.source {
        SkillSelectionSource::Explicit => TargetSelection::Explicit,
        SkillSelectionSource::All => TargetSelection::All,
        SkillSelectionSource::Picker => TargetSelection::Picker,
        SkillSelectionSource::Detected => TargetSelection::Detected,
        SkillSelectionSource::Fallback => TargetSelection::Fallback,
    });
    telemetry.resolved_agents = Some(count_bucket(selection.agents.len() as u64));
}

fn status_result_json(result: &StatusResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "status": result.status.as_str(),
        "path": result.target.skill_dir,
        "installed_hash": result.installed_hash,
        "bundled_hash": bundled_hash(),
        "metadata": result.metadata.as_ref().map(|metadata| json!({
            "schema_version": metadata.schema_version,
            "skill_name": metadata.skill_name,
            "skill_hash": metadata.skill_hash,
            "ctx_cli_version": metadata.ctx_cli_version,
        })),
    })
}

fn install_result_json(result: &InstallResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.skill_dir,
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_installed": result.already_installed,
        "updated": result.updated,
        "error": result.error,
    })
}

fn render_install_results(context: &RenderContext, results: &[InstallResult]) -> Document {
    let all_success = !results.is_empty() && results.iter().all(|result| result.success);
    let all_current = all_success && results.iter().all(|result| result.already_installed);
    let any_updated = results
        .iter()
        .any(|result| result.success && result.updated);
    let any_installed = results
        .iter()
        .any(|result| result.success && !result.already_installed && !result.updated);
    let title = if all_current {
        "Agent skill is already installed"
    } else if all_success && any_updated && !any_installed {
        "Agent skill updated"
    } else if all_success {
        "Agent skill installed"
    } else {
        "Agent skill needs attention"
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
    document.push_blank();
    document.append(fields(context, &[Field::new("Skill", BUNDLED_SKILL_NAME)]));

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_installed {
                "current"
            } else if !result.success {
                "skipped"
            } else if result.updated {
                "updated"
            } else {
                "installed"
            };
            (status, result.target.agent.display_name().to_owned())
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

fn render_install_failures(context: &RenderContext, results: &[InstallResult]) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} Agent Skill was not changed",
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
                action: (result.status == SkillInstallStatus::Modified)
                    .then_some(Action { command: &command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

fn force_install_command(target: &SkillTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "ctx integrations install skills --agent {}{project} --force",
        target.agent.id()
    )
}

fn render_status_results(
    context: &RenderContext,
    results: &[StatusResult],
    recovery_command: &str,
) -> Document {
    let all_current = !results.is_empty()
        && results
            .iter()
            .all(|result| result.status == SkillInstallStatus::Current);
    let mut document = outcome(
        context,
        Outcome {
            state: if all_current {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: if all_current {
                "Agent skill is current"
            } else {
                "Agent skill needs attention"
            },
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(context, &[Field::new("Skill", BUNDLED_SKILL_NAME)]));

    let rows = results
        .iter()
        .map(|result| {
            (
                result.status.as_str(),
                format!(
                    "{} ({}) -> {}",
                    result.target.agent.display_name(),
                    result.target.scope.as_str(),
                    result.target.skill_dir.display()
                ),
            )
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    if !all_current {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Install or refresh the bundled Agent Skill for the affected targets.",
            },
            Some(Action {
                command: recovery_command,
            }),
        ));
    }
    document
}

fn status_install_command(
    selection: &SkillAgentSelection,
    project: bool,
    results: &[StatusResult],
) -> String {
    let mut tokens = ["ctx", "integrations", "install", "skills"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if selection.source == SkillSelectionSource::All {
        tokens.push("--all-agents".to_owned());
    } else {
        for agent in &selection.agents {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    }
    if project {
        tokens.push("--project".to_owned());
    }
    if results
        .iter()
        .any(|result| result.status == SkillInstallStatus::Modified)
    {
        tokens.push("--force".to_owned());
    }
    tokens.join(" ")
}

#[cfg(test)]
mod render_tests {
    use std::io::Write as _;

    use super::*;
    use crate::{
        skill::agents::SkillAgentArg,
        ui::{ColorMode, StreamKind, TestContext, Token},
    };
    use ctx_agent_integrations::skill::{
        install_target, resolve_targets_for_agents, status_target,
    };

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
    fn human_install_and_status_results_use_the_typed_ui() {
        let temp = tempfile::tempdir().unwrap();
        let path_context = super::super::paths::PathContext::for_tests(
            temp.path().join("home"),
            temp.path().join("repo"),
        );
        let target = resolve_targets_for_agents(&[SkillAgentArg::Universal], false, &path_context)
            .unwrap()
            .remove(0);
        let missing = status_target(&target).unwrap();
        let installed = install_target(&target, false, true, env!("CARGO_PKG_VERSION")).unwrap();
        let current = status_target(&target).unwrap();

        for (document, expected) in [
            (
                render_install_results(&render_context(80, ColorMode::Never), &[installed]),
                "Agent skill installed",
            ),
            (
                render_status_results(
                    &render_context(80, ColorMode::Never),
                    &[missing],
                    "ctx integrations install skills --agent universal",
                ),
                "Agent skill needs attention",
            ),
            (
                render_status_results(&render_context(80, ColorMode::Never), &[current], "unused"),
                "Agent skill is current",
            ),
        ] {
            let plain = document.render_plain();
            assert!(plain.contains(expected), "{plain}");
            assert!(plain.contains("Skill"), "{plain}");
            assert!(plain.contains("Targets"), "{plain}");
        }

        let color = render_context(80, ColorMode::Always);
        let document = render_status_results(&color, &[status_target(&target).unwrap()], "unused");
        let styled = document.render(&color);
        assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
        assert_eq!(strip_ansi(&styled), document.render_plain());
    }

    #[test]
    fn missing_skill_status_offers_the_exact_selected_install_action() {
        let temp = tempfile::tempdir().unwrap();
        let path_context = super::super::paths::PathContext::for_tests(
            temp.path().join("home"),
            temp.path().join("repo"),
        );
        let selection = SkillAgentSelection {
            agents: vec![SkillAgentArg::Universal],
            source: SkillSelectionSource::Explicit,
        };
        let target = resolve_targets_for_agents(&selection.agents, true, &path_context)
            .unwrap()
            .remove(0);
        let result = status_target(&target).unwrap();
        let command = status_install_command(&selection, true, std::slice::from_ref(&result));
        assert_eq!(
            command,
            "ctx integrations install skills --agent universal --project"
        );

        for width in [32, 48, 80, 120] {
            let context = render_context(width, ColorMode::Never);
            let document = render_status_results(&context, std::slice::from_ref(&result), &command);
            assert_eq!(semantic_command(&document), command);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains("Install or refresh the bundled Agent Skill"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn modified_skill_failure_names_the_selected_agent_in_the_force_action() {
        let temp = tempfile::tempdir().unwrap();
        let path_context = super::super::paths::PathContext::for_tests(
            temp.path().join("home"),
            temp.path().join("repo"),
        );

        for project in [false, true] {
            let target =
                resolve_targets_for_agents(&[SkillAgentArg::Universal], project, &path_context)
                    .unwrap()
                    .remove(0);
            let result = InstallResult {
                target,
                success: false,
                fatal: true,
                previous_status: SkillInstallStatus::Modified,
                status: SkillInstallStatus::Modified,
                already_installed: false,
                updated: false,
                error: Some("preserved an existing skill; use --force to replace".to_owned()),
            };
            let expected_project = if project { " --project" } else { "" };
            let expected = format!(
                "ctx integrations install skills --agent universal{expected_project} --force"
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
                assert!(normalized.contains("Universal .agents Agent Skill was not changed"));
                assert_eq!(
                    strip_ansi(&styled_document.render(&styled_context)),
                    plain_document.render_plain()
                );
            }
        }
    }
}
