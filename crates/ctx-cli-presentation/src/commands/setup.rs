use std::path::Path;

use clap::Args;
use serde_json::Value;

use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token,
};
use crate::{output::JsonOutputFormat, progress::ProgressArg};

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(
        long,
        alias = "no-import",
        help = "Deprecated and ignored; setup follows its normal refresh lifecycle"
    )]
    pub catalog_only: bool,
    #[arg(
        long,
        help = "Enable local semantic search in config (requires daemon maintenance)"
    )]
    pub semantic: bool,
    #[arg(long, help = "Do not start daemon maintenance after setup")]
    pub no_daemon: bool,
    #[arg(
        long,
        help = "Set up the anonymous ctx Pro trial while Core indexing begins"
    )]
    pub pro: bool,
    #[arg(
        long,
        help = "Wait for the daemon-owned lexical refresh to publish before returning"
    )]
    pub wait: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub progress: ProgressArg,
}

pub struct SetupDaemonState<'a> {
    pub requested: bool,
    pub reason: Option<&'a str>,
    pub started: bool,
    pub persistent_supervisor_verified: bool,
}

pub fn render_setup_human(
    context: &RenderContext,
    data_root: &Path,
    mode: &str,
    source: &Value,
    refresh_request: &Value,
    daemon: SetupDaemonState<'_>,
    pro: &Value,
) -> Document {
    let refresh_status = refresh_request["status"].as_str().unwrap_or("unavailable");
    let queued = mode == "pending"
        || matches!(
            refresh_status,
            "accepted" | "pending" | "queued" | "running"
        );
    let (state, title, detail) = if mode == "ready" {
        (
            OutcomeState::Success,
            "History is ready to search",
            queued.then_some("A refresh is running; the current index remains searchable."),
        )
    } else if queued {
        (
            OutcomeState::Neutral,
            "History indexing is queued",
            Some("Background indexing will publish the first searchable index."),
        )
    } else {
        (
            OutcomeState::Warning,
            "History is not ready",
            Some("Setup completed, but no verified search index is available."),
        )
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail,
        },
    );

    let source_count = source["indexed_sources"]
        .as_u64()
        .or_else(|| source["lexical"]["certified_sources"].as_u64())
        .or_else(|| source["refresh"]["certified_source_count"].as_u64())
        .or_else(|| refresh_request["source_count"].as_u64());
    let mut history_values = Vec::new();
    if let Some(count) = source_count {
        history_values.push(("Sources", counted(count, "source", "sources").to_owned()));
    }
    if let Some(count) = source["indexed_sessions"].as_u64() {
        history_values.push(("Sessions", counted(count, "session", "sessions")));
    }
    let event_count = source["indexed_events"]
        .as_u64()
        .or_else(|| source["lexical"]["indexed_documents"].as_u64());
    if let Some(count) = event_count {
        history_values.push((
            "Events",
            counted(count, "searchable event", "searchable events"),
        ));
    }
    history_values.push(("Semantic", component_status(&source["semantic"]).to_owned()));
    if let Some(status) = daemon_human_status(&daemon) {
        history_values.push(("Background", status));
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));

    if mode != "ready" && !queued {
        let data_root = data_root.display().to_string();
        document.push_blank();
        document.append(section(
            "Data",
            fields(context, &[Field::new("Root", &data_root)]),
        ));
    }

    append_pro_setup(&mut document, pro);

    let next_command = if mode == "ready" {
        "ctx search \"test failure\""
    } else if queued {
        "ctx index watch"
    } else if daemon.requested && !daemon.started {
        "ctx daemon status"
    } else if matches!(daemon.reason, Some("daemon_disabled" | "explicit_opt_out")) {
        "ctx daemon enable"
    } else {
        "ctx doctor"
    };
    document.push_blank();
    document.append(section(
        "Next",
        Document::from_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(next_command, Token::Command)),
        ),
    ));
    document
}

fn append_pro_setup(document: &mut Document, pro: &Value) {
    match pro["status"].as_str() {
        Some("ready") if pro["trial_started"].as_bool() == Some(true) => {
            document.push_blank();
            document.push_line(Line::text(
                "ctx pro is a US$20/mo paid add-on for “git blame, but for agent sessions.”",
            ));
            document.push_line(Line::text(
                "Not a cloud service — your agent history stays local.",
            ));
            document.push_line(Line::text(
                "You get a 14-day free trial — no account or card required.",
            ));
            let keep = pro["trial_ends_on"]
                .as_str()
                .map(|date| format!("To keep it after {date}: run ctx pro"))
                .unwrap_or_else(|| "To keep it after the trial: run ctx pro".to_owned());
            document.push_line(Line::new().with(Span::text(keep)));
        }
        Some("unavailable") => {
            document.push_blank();
            document.push_line(Line::text(
                "ctx pro could not be activated; Core history setup continues normally.",
            ));
            document.push_line(
                Line::new()
                    .with(Span::text("Retry: "))
                    .with(Span::new("ctx pro", Token::Command)),
            );
        }
        _ => {}
    }
}

fn component_status(component: &Value) -> &str {
    component
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
}

fn daemon_human_status(daemon: &SetupDaemonState<'_>) -> Option<String> {
    match (daemon.started, daemon.persistent_supervisor_verified) {
        (true, true) => None,
        (true, false) => Some("persistent daemon (automatic restart unavailable)".to_owned()),
        (false, _) if daemon.requested => {
            Some("startup was not verified; run ctx daemon status".to_owned())
        }
        (false, _) if daemon.reason == Some("explicit_opt_out") => {
            Some("skipped because --no-daemon was used".to_owned())
        }
        (false, _) if daemon.reason == Some("daemon_disabled") => Some("disabled".to_owned()),
        (false, _) => None,
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;
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

    fn ready_source() -> Value {
        json!({
            "indexed_sources": 1,
            "indexed_sessions": 2,
            "indexed_events": 1000,
            "lexical": {
                "status": "ready",
                "certified_sources": 9,
                "indexed_documents": 9,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        })
    }

    fn skipped_pro() -> Value {
        json!({"status": "skipped", "trial_started": false})
    }

    fn render_ready(context: &RenderContext) -> Document {
        render_setup_human(
            context,
            Path::new("/tmp/ctx"),
            "ready",
            &ready_source(),
            &json!({"status": "published"}),
            SetupDaemonState {
                requested: false,
                reason: None,
                started: false,
                persistent_supervisor_verified: true,
            },
            &skipped_pro(),
        )
    }

    #[test]
    fn setup_ready_is_outcome_first_and_has_one_search_action() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_ready(&context);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ History is ready to search\n\nHistory\n"));
            assert!(rendered.contains("Sources   1 source\n"));
            assert!(rendered.contains("Sessions  2 sessions\n"));
            assert!(normalized.contains("Events 1,000 searchable events"));
            assert!(!rendered.contains("9 sources"));
            assert!(!rendered.contains("9 searchable events"));
            assert!(rendered.contains("Next\n  ctx search \"test failure\"\n"));
            assert!(!rendered.contains("Generation"));
            assert!(!rendered.contains("PID"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_eq!(rendered.matches("\nNext\n").count(), 1);
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_events_fall_back_to_the_equivalent_lexical_document_count() {
        let source = json!({
            "lexical": {
                "status": "ready",
                "indexed_documents": 1,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        });
        let document = render_setup_human(
            &context(80, ColorMode::Never),
            Path::new("/tmp/ctx"),
            "ready",
            &source,
            &json!({"status": "published"}),
            SetupDaemonState {
                requested: false,
                reason: None,
                started: false,
                persistent_supervisor_verified: true,
            },
            &skipped_pro(),
        );
        let rendered = document.render_plain();
        assert!(rendered.contains("Events    1 searchable event\n"));
        assert!(!rendered.contains("Sessions"));
    }

    #[test]
    fn setup_queued_has_watch_as_its_primary_action_without_an_eta() {
        let source = json!({
            "lexical": {"status": "pending"},
            "refresh": {"status": "pending"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({"status": "pending"});
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "pending",
                &source,
                &refresh,
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: true,
                },
                &skipped_pro(),
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("History indexing is queued\n"));
            assert!(!rendered.contains("Refresh"));
            assert!(rendered.contains("Next\n  ctx index watch\n"));
            assert!(!rendered.contains("Estimated"));
            assert!(!rendered.contains("ctx search"));
            assert!(!rendered.contains("42"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_degraded_explains_disabled_background_work_and_recovery() {
        let source = json!({
            "lexical": {"status": "unavailable"},
            "refresh": {"status": "unavailable"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({
            "status": "unavailable",
            "reason": "daemon_disabled",
        });
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "unavailable",
                &source,
                &refresh,
                SetupDaemonState {
                    requested: false,
                    reason: Some("daemon_disabled"),
                    started: false,
                    persistent_supervisor_verified: false,
                },
                &skipped_pro(),
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! History is not ready\n"));
            assert!(rendered.contains("Background  disabled\n"));
            assert!(rendered.contains("Data\nRoot  /tmp/ctx\n"));
            assert!(rendered.contains("Next\n  ctx daemon enable\n"));
            assert!(!rendered.contains("ctx index watch"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_manager_unavailable_reports_a_persistent_daemon_without_native_restart() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "ready",
                &ready_source(),
                &json!({"status": "published"}),
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: false,
                },
                &skipped_pro(),
            );
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ History is ready to search\n"));
            assert!(
                normalized.contains("Background persistent daemon (automatic restart unavailable)")
            );
            assert!(!normalized.contains("Continuous refresh is unavailable"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_fallback_reports_a_persistent_daemon_without_a_bounded_limitation() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "ready",
                &ready_source(),
                &json!({"status": "published"}),
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: false,
                },
                &skipped_pro(),
            );
            let normalized = document
                .render_plain()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                normalized.contains("Background persistent daemon (automatic restart unavailable)")
            );
            assert!(!normalized.contains("Continuous refresh is unavailable"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_plain_output_equals_ansi_stripped_styled_output() {
        let context = context(80, ColorMode::Always);
        let document = render_ready(&context);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }

    #[test]
    fn setup_pro_trial_disclosure_is_clear_and_contains_no_fake_url() {
        let context = context(80, ColorMode::Never);
        let document = render_setup_human(
            &context,
            Path::new("/tmp/ctx"),
            "ready",
            &ready_source(),
            &json!({"status": "published"}),
            SetupDaemonState {
                requested: true,
                reason: None,
                started: true,
                persistent_supervisor_verified: true,
            },
            &json!({
                "status": "ready",
                "trial_started": true,
                "trial_ends_on": "2026-08-28",
                "action_url": null,
            }),
        );
        let rendered = document.render_plain();
        assert!(rendered.contains("ctx pro is a US$20/mo paid add-on"));
        assert!(rendered.contains("your agent history stays local"));
        assert!(rendered.contains("14-day free trial"));
        assert!(rendered.contains("To keep it after 2026-08-28: run ctx pro"));
        assert!(!rendered.contains("https://"));
        assert_fits(&document, &context);
    }
}
