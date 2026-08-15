use std::path::Path;

use ctx_client_observability::local_usage;
use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::progress::format_bytes;
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token,
};

use super::status_health::*;

#[allow(clippy::too_many_arguments)]
pub fn render_status_human(
    context: &RenderContext,
    report: &Value,
    data_root: &Path,
    config_path: &Path,
    upgrade: &Value,
    pro: &Value,
    local_usage: &local_usage::UsageReport,
    pro_monthly_price_display: &str,
) -> Document {
    let history_health = history_health(report);
    let service_issues = actionable_service_issue_count(report, upgrade, local_usage);
    let health = if history_health == StatusHealth::Healthy && service_issues > 0 {
        StatusHealth::Partial
    } else {
        history_health
    };
    let unhealthy = unhealthy_history_components(report);
    let pending = unhealthy
        .iter()
        .filter(|(_, component)| component_status(component) == "pending")
        .count();
    let lexical_status = component_status(&report["lexical"]);
    let (state, title, detail) = match health {
        StatusHealth::Healthy => (OutcomeState::Success, "ctx is healthy", None),
        StatusHealth::Partial if history_health == StatusHealth::Healthy => (
            OutcomeState::Warning,
            "ctx needs attention",
            Some(format!(
                "{}; search remains available.",
                auxiliary_attention_message(service_issues)
            )),
        ),
        StatusHealth::Partial if lexical_status == "pending" => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some("History indexing is in progress.".to_owned()),
        ),
        StatusHealth::Partial if pending == unhealthy.len() => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some(format!(
                "{}; search remains available.",
                service_progress_message(unhealthy.len())
            )),
        ),
        StatusHealth::Partial => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some(format!(
                "{}; search remains available.",
                service_attention_message(unhealthy.len())
            )),
        ),
        StatusHealth::Failed => (
            OutcomeState::Error,
            "History status: failed",
            Some("A verified search index is not available.".to_owned()),
        ),
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail: detail.as_deref(),
        },
    );

    let mut history_values = vec![("Search", component_display(&report["lexical"]).to_owned())];
    for (label, field, singular, plural) in [
        (
            "Sources",
            "indexed_sources",
            "indexed source",
            "indexed sources",
        ),
        (
            "Sessions",
            "indexed_sessions",
            "indexed session",
            "indexed sessions",
        ),
        (
            "Events",
            "indexed_events",
            "searchable event",
            "searchable events",
        ),
    ] {
        if let Some(count) = report[field].as_u64() {
            history_values.push((label, counted(count, singular, plural)));
        }
    }
    let rejected_records = current_rejected_record_count(report);
    if rejected_records > 0 {
        history_values.push((
            "Rejected",
            counted(rejected_records, "provider record", "provider records"),
        ));
    }
    if history_health == StatusHealth::Healthy {
        history_values.push(("Refresh", component_display(&report["refresh"]).to_owned()));
    } else {
        for (label, component) in supporting_history_components(report) {
            if !component_is_healthy(component) {
                history_values.push((label, component_display(component)));
            }
        }
    }
    if component_status(&report["refresh"]) == "pending" {
        let progress = report["refresh"].get("progress");
        if let Some(source) = progress
            .and_then(|progress| progress.get("current_source"))
            .and_then(Value::as_str)
            .filter(|source| !source.is_empty())
        {
            history_values.push(("Active source", source.to_owned()));
        }
        if let Some(records) = progress
            .and_then(|progress| progress.get("completed_records"))
            .and_then(Value::as_u64)
        {
            history_values.push(("Accepted", counted(records, "record", "records")));
        }
        if let Some(bytes) = progress
            .and_then(|progress| progress.get("completed_bytes"))
            .and_then(Value::as_u64)
        {
            history_values.push(("Scanned", format_bytes(bytes)));
        }
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));

    let daemon = &report["daemon"];
    let daemon_status = if daemon.get("running").and_then(Value::as_bool) == Some(true) {
        "running".to_owned()
    } else {
        component_display(daemon)
    };
    let mut service_values = vec![
        ("Daemon", daemon_status),
        (
            "Semantic",
            component_display(&report["semantic"]).to_owned(),
        ),
    ];
    if let Some(issue) = upgrade_service_issue(upgrade) {
        service_values.push(("Automatic upgrades", issue));
    }
    service_values.push(("Local usage", local_usage_display(local_usage)));
    let service_fields = service_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Services", fields(context, &service_fields)));

    if pro["installed"].as_bool() == Some(true) {
        let mut pro_values = vec![(
            "Status",
            pro["state"].as_str().unwrap_or("unavailable").to_owned(),
        )];
        if let Some(access) = pro["access_state"].as_str() {
            pro_values.push(("Access", humanize_code(access)));
        }
        if let Some(action) = pro["conversion_action"].as_object() {
            if action.get("kind").and_then(Value::as_str) == Some("pro_restore_access") {
                pro_values.push(("Data", "graph preserved".to_owned()));
            } else {
                pro_values.push((
                    "Upgrade",
                    action
                        .get("price")
                        .and_then(Value::as_str)
                        .unwrap_or(pro_monthly_price_display)
                        .to_owned(),
                ));
            }
            pro_values.push((
                "Next",
                action
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("ctx pro manage")
                    .to_owned(),
            ));
        } else if pro["state"].as_str() != Some("ready") {
            if let Some(command) = pro["next_action"]["command"].as_str() {
                pro_values.push(("Next", command.to_owned()));
            }
        }
        let pro_fields = pro_values
            .iter()
            .map(|(label, value)| Field::new(label, value.as_str()))
            .collect::<Vec<_>>();
        document.push_blank();
        document.append(section("Pro", fields(context, &pro_fields)));
    }

    if health == StatusHealth::Failed {
        let data_root = data_root.display().to_string();
        let config_path = config_path.display().to_string();
        document.push_blank();
        document.append(section(
            "Data",
            fields(
                context,
                &[
                    Field::new("Root", &data_root),
                    Field::new("Config", &config_path),
                ],
            ),
        ));
    }

    if let Some(command) = status_next_command(report, health, pending, unhealthy.len()) {
        document.push_blank();
        document.append(section(
            "Next",
            Document::from_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(command, Token::Command)),
            ),
        ));
    }
    document
}

pub(super) fn humanize_code(value: &str) -> String {
    match value {
        "projection_missing" => "still being prepared".to_owned(),
        "generation_not_published" => "history has not been indexed yet".to_owned(),
        "lexical_generation_unavailable" => "search index unavailable".to_owned(),
        _ => value.replace('_', " "),
    }
}

fn counted(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", grouped_count(count))
}

fn grouped_count(count: u64) -> String {
    let digits = count.to_string();
    let mut reversed = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(character);
    }
    reversed.chars().rev().collect()
}

fn current_rejected_record_count(report: &Value) -> u64 {
    report
        .get("refresh")
        .and_then(|refresh| refresh.get("current"))
        .and_then(|current| current.get("current_rejected_records"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn status_report(initialized: bool, lexical: &str, refresh: &str) -> Value {
        json!({
            "initialized": initialized,
            "history_epoch": {"status": lexical},
            "lexical": {"status": lexical},
            "refresh": {"status": refresh},
            "semantic": {"status": "disabled"},
            "daemon": {"status": "running", "running": true},
            "indexed_sources": 1,
            "indexed_sessions": 2,
            "indexed_events": 1000,
        })
    }

    fn usage_report() -> local_usage::UsageReport {
        local_usage::UsageReport {
            schema_version: 2,
            local_only: true,
            read_only: true,
            enabled: true,
            state: "ready",
            retention_days: 400,
            definition_version: 2,
            definitions: None,
            estimates: None,
            error: None,
        }
    }

    fn no_pro() -> Value {
        json!({"installed": false})
    }

    fn healthy_upgrade() -> Value {
        json!({
            "auto": "apply",
            "install": {
                "managed": false,
                "marker": "absent",
            },
        })
    }

    fn render_report(context: &RenderContext, report: &Value) -> Document {
        render_status_human(
            context,
            report,
            std::path::Path::new("/tmp/ctx"),
            std::path::Path::new("/tmp/ctx/config.toml"),
            &healthy_upgrade(),
            &no_pro(),
            &usage_report(),
            "$20/month",
        )
    }

    #[test]
    fn healthy_status_is_concise_and_hides_routine_internal_paths() {
        let report = status_report(true, "ready", "ready");
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ ctx is healthy\n\nHistory\n"));
            assert!(rendered.contains("1 indexed source"));
            assert!(rendered.contains("2 indexed sessions"));
            assert!(normalized.contains("1,000 searchable events"));
            assert!(rendered.contains("\nServices\n"));
            assert!(normalized.contains("Daemon running"));
            assert!(normalized.contains("Semantic disabled"));
            assert!(!rendered.contains("Automatic upgrades"));
            assert!(rendered.contains("Local usage"));
            assert!(normalized.contains("enabled; store readable"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert!(!rendered.contains("local-only"));
            assert!(!rendered.contains("read-only"));
            assert!(!rendered.contains("Generation"));
            assert!(!rendered.contains("PID"));
            assert!(!rendered.contains("\nNext\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn published_rejections_keep_status_healthy_and_visible() {
        let mut report = status_report(true, "ready", "ready");
        report["refresh"]["current"] = json!({
            "current_rejected_records": 3,
            "current_sources_with_rejections": 1,
        });
        let rendered = render_report(&context(80, ColorMode::Never), &report).render_plain();

        assert!(rendered.starts_with("✓ ctx is healthy\n"), "{rendered}");
        assert!(rendered.contains("Refresh   ready"), "{rendered}");
        assert!(
            rendered.contains("Rejected  3 provider records"),
            "{rendered}"
        );
        assert!(!rendered.contains("partially ready"), "{rendered}");
        assert!(!rendered.contains("\nNext\n"), "{rendered}");
    }

    #[test]
    fn status_ignores_legacy_path_precedence_diagnostics() {
        let report = status_report(true, "ready", "ready");
        let upgrade = json!({
            "auto": "apply",
            "install": {
                "managed": true,
                "marker": "valid",
                "path": {
                    "background_apply": {
                        "allowed": false,
                        "reason": "path_shadowed",
                    },
                },
            },
        });
        let usage = local_usage::UsageReport::config_error();
        let context = context(80, ColorMode::Never);
        let document = render_status_human(
            &context,
            &report,
            std::path::Path::new("/tmp/ctx"),
            std::path::Path::new("/tmp/ctx/config.toml"),
            &upgrade,
            &no_pro(),
            &usage,
            "$20/month",
        );
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.starts_with("! ctx needs attention\n"));
        assert!(!normalized.contains("Automatic upgrades"), "{rendered}");
        assert!(!normalized.contains("PATH"), "{rendered}");
        assert!(rendered.contains("Local usage"));
        assert!(rendered.contains("local_usage_config_unavailable"));
        assert!(normalized.contains("local usage configuration could not be read"));
        assert_fits(&document, &context);
    }

    #[test]
    fn actionable_daemon_or_semantic_state_prevents_a_healthy_headline() {
        let mut daemon_failed = status_report(true, "ready", "ready");
        daemon_failed["daemon"] = json!({
            "status": "failed",
            "enabled": true,
            "running": false,
            "reason": "daemon_process_failed",
        });
        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &daemon_failed).render_plain();
        assert!(
            rendered.starts_with("! ctx needs attention\n"),
            "{rendered}"
        );
        assert!(rendered.contains("Daemon"), "{rendered}");
        assert!(rendered.contains("failed"), "{rendered}");
        assert!(rendered.contains("Next\n  ctx doctor\n"), "{rendered}");

        let mut semantic_pending = status_report(true, "ready", "ready");
        semantic_pending["semantic"] = json!({
            "status": "pending",
            "enabled": true,
            "reason": "projection_missing",
        });
        let rendered = render_report(&context, &semantic_pending).render_plain();
        assert!(
            rendered.starts_with("! ctx needs attention\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("pending (still being prepared)"),
            "{rendered}"
        );
        assert!(!rendered.contains("projection missing"), "{rendered}");
    }

    #[test]
    fn normal_status_always_reports_local_usage_enablement_and_store_health() {
        let report = status_report(true, "ready", "ready");
        let context = context(80, ColorMode::Never);
        for (usage, expected) in [
            (
                local_usage::UsageReport {
                    enabled: false,
                    state: "disabled",
                    ..usage_report()
                },
                "Local usage  disabled",
            ),
            (
                local_usage::UsageReport {
                    state: "empty",
                    ..usage_report()
                },
                "Local usage  enabled; store missing or empty",
            ),
        ] {
            let rendered = render_status_human(
                &context,
                &report,
                std::path::Path::new("/tmp/ctx"),
                std::path::Path::new("/tmp/ctx/config.toml"),
                &healthy_upgrade(),
                &no_pro(),
                &usage,
                "$20/month",
            )
            .render_plain();
            assert!(rendered.contains(expected), "{rendered}");
        }
    }

    #[test]
    fn partial_status_keeps_searchable_counts_and_points_to_index_watch() {
        let report = status_report(true, "ready", "pending");
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx is partially ready\n"));
            assert!(rendered.contains("Refresh"));
            assert!(rendered.contains("pending"));
            assert!(normalized.contains("1,000 searchable events"));
            assert!(rendered.contains("Next\n  ctx index watch\n"));
            assert!(!rendered.contains("\nData\n"));
            assert_fits(&document, &context);
        }

        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &report).render_plain();
        assert!(rendered.contains("1 history service is catching up; search remains available."));
    }

    #[test]
    fn active_refresh_status_shows_record_and_byte_progress_without_inventing_a_total() {
        let mut report = status_report(true, "ready", "pending");
        report["refresh"]["progress"] = json!({
            "phase": "refreshing",
            "completed_sources": 2,
            "total_sources": 6,
            "current_source": "~/.local/share/opencode/opencode.db",
            "completed_records": 1234,
            "completed_bytes": 4 * 1024 * 1024,
        });

        let rendered = render_report(&context(80, ColorMode::Never), &report).render_plain();
        assert!(rendered.contains("Active source  ~/.local/share/opencode/opencode.db"));
        assert!(rendered.contains("Accepted       1,234 records"));
        assert!(rendered.contains("Scanned        4.0 MiB"));
        assert!(!rendered.contains("records /"));
    }

    #[test]
    fn failed_status_exposes_actionable_paths_and_doctor_recovery() {
        let mut report = status_report(false, "unavailable", "unavailable");
        report["lexical"]["reason"] = json!("generation_verification_failed");
        report["daemon"] = json!({
            "status": "failed",
            "reason": "daemon_process_failed",
            "running": false,
        });

        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✗ History status: failed\n"));
            assert!(normalized.contains("generation verification failed"));
            assert!(rendered.contains("\nData\n"));
            assert!(rendered.contains("/tmp/ctx"));
            assert!(rendered.contains("Next\n  ctx doctor\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn status_without_a_published_generation_points_to_setup() {
        let mut report = status_report(false, "unavailable", "unavailable");
        report["lexical"]["reason"] = json!("generation_not_published");
        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &report).render_plain();
        assert!(rendered.contains("Next\n  ctx setup\n"));
    }

    #[test]
    fn installed_pro_status_is_grouped_without_internal_deadlines() {
        let report = status_report(true, "ready", "ready");
        let pro = json!({
            "installed": true,
            "state": "ready",
            "access_state": "trial",
            "refresh_after_unix": 100,
            "access_deadline_unix": 200,
            "conversion_action": {
                "kind": "pro_monthly_conversion",
                "price": "$20/month",
                "command": "ctx pro manage",
            },
        });

        for width in [32, 80] {
            let context = context(width, ColorMode::Never);
            let document = render_status_human(
                &context,
                &report,
                std::path::Path::new("/tmp/ctx"),
                std::path::Path::new("/tmp/ctx/config.toml"),
                &healthy_upgrade(),
                &pro,
                &usage_report(),
                "$20/month",
            );
            let rendered = document.render_plain();
            assert!(rendered.contains("\nPro\n"));
            assert!(rendered.contains("$20/month"));
            assert!(rendered.contains("ctx pro manage"));
            assert!(!rendered.contains("100"));
            assert!(!rendered.contains("200"));
            assert!(!rendered.contains("deadline"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn status_plain_output_equals_ansi_stripped_styled_output() {
        let report = status_report(true, "ready", "ready");
        let context = context(80, ColorMode::Always);
        let document = render_report(&context, &report);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
