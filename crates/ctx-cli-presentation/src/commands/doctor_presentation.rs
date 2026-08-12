use serde_json::Value;

use crate::ui::{
    evidence_list, fields, hint, outcome, section, Action, Document, Evidence, Field, Hint,
    Outcome, OutcomeState, RenderContext,
};

pub fn source_epoch_findings(
    report: &Value,
    semantic_required: bool,
    pro_projection_required: bool,
) -> Vec<String> {
    let mut findings = Vec::new();
    for (name, required) in [
        ("history_epoch", true),
        ("lexical", true),
        ("catalog", true),
        ("refresh", true),
        ("semantic", semantic_required),
        ("pro_projection", pro_projection_required),
    ] {
        if !required {
            continue;
        }
        let component = &report[name];
        let status = component
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable");
        if !matches!(status, "ready" | "disabled") {
            let reason = component
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            findings.push(format!("{name} is {status} ({reason})"));
        }
    }
    findings
}

pub fn render_doctor_human(
    context: &RenderContext,
    automatic_upgrades: &str,
    findings: &[String],
    rejected_records: u64,
) -> Document {
    let title = match findings.len() {
        0 => "No problems found".to_owned(),
        1 => "ctx found 1 issue".to_owned(),
        count => format!("ctx found {count} issues"),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if findings.is_empty() {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: &title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Configuration",
        fields(
            context,
            &[Field::new("Automatic upgrades", automatic_upgrades)],
        ),
    ));
    if rejected_records > 0 {
        document.push_blank();
        document.append(section(
            "History",
            fields(
                context,
                &[Field::new(
                    "Rejected",
                    &format!(
                        "{} provider record{}",
                        rejected_records,
                        if rejected_records == 1 { "" } else { "s" }
                    ),
                )],
            ),
        ));
    }
    if findings.is_empty() {
        return document;
    }

    let human_findings = findings
        .iter()
        .map(|finding| humanize_doctor_finding(finding))
        .collect::<Vec<_>>();
    let references = (1..=human_findings.len())
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    let evidence = references
        .iter()
        .zip(&human_findings)
        .map(|(reference, finding)| Evidence {
            reference,
            summary: &finding.summary,
            detail: finding.detail.as_deref(),
        })
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Issues", evidence_list(context, &evidence)));
    document.push_blank();
    let refresh_failed = findings.iter().any(|finding| {
        finding.contains("(source_refresh_failed)") || finding.contains("(core_refresh_failed)")
    });
    document.append(hint(
        context,
        Hint {
            text: if refresh_failed {
                "Check the history refresh service."
            } else {
                "Resolve the issues above, then check again."
            },
        },
        Some(Action {
            command: if refresh_failed {
                "ctx daemon status"
            } else {
                "ctx doctor"
            },
        }),
    ));
    document
}

struct HumanDoctorFinding {
    summary: String,
    detail: Option<String>,
}

fn humanize_doctor_finding(finding: &str) -> HumanDoctorFinding {
    let Some((component, state_and_reason)) = finding.split_once(" is ") else {
        return HumanDoctorFinding {
            summary: finding.to_owned(),
            detail: None,
        };
    };
    let Some((state, reason)) = state_and_reason
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once(" ("))
    else {
        return HumanDoctorFinding {
            summary: finding.to_owned(),
            detail: None,
        };
    };
    let label = match component {
        "history_epoch" => "History",
        "lexical" => "Search index",
        "catalog" => "History source catalog",
        "refresh" => "History refresh",
        "semantic" => "Semantic search",
        "pro_projection" => "ctx Pro index",
        _ => {
            return HumanDoctorFinding {
                summary: finding.to_owned(),
                detail: None,
            }
        }
    };
    let summary = match state {
        "pending" => format!("{label} is still preparing"),
        "unavailable" => format!("{label} is unavailable"),
        other => format!("{label} is {}", other.replace('_', " ")),
    };
    let detail = match reason {
        "catalog_publication_pending" => "Required local data is still being prepared.",
        "daemon_unavailable" => "The background history refresh service is not available.",
        "source_refresh_failed" | "core_refresh_failed" | "lexical_generation_unavailable" => {
            "Required local data is not available."
        }
        _ => "The component is not ready.",
    };
    HumanDoctorFinding {
        summary,
        detail: Some(detail.to_owned()),
    }
}

#[cfg(test)]
mod ui_tests {
    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    #[test]
    fn healthy_doctor_is_concise_and_outcome_first() {
        let context = context(80);
        let rendered = render_doctor_human(&context, "apply", &[], 0).render_plain();
        assert_eq!(
            rendered,
            "✓ No problems found\n\nConfiguration\nAutomatic upgrades  apply\n"
        );
    }

    #[test]
    fn healthy_doctor_discloses_rejected_provider_records() {
        let rendered = render_doctor_human(&context(80), "apply", &[], 2).render_plain();

        assert!(rendered.starts_with("✓ No problems found\n"), "{rendered}");
        assert!(
            rendered.contains("History\nRejected  2 provider records\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("issue"), "{rendered}");
    }

    #[test]
    fn doctor_ignores_record_diagnostics_but_flags_source_failures() {
        let base = json!({
            "history_epoch": {"status": "ready"},
            "lexical": {"status": "ready"},
            "catalog": {"status": "ready"},
            "semantic": {"status": "disabled"},
            "pro_projection": {"status": "unavailable"},
        });
        let mut rejections = base.clone();
        rejections["refresh"] = json!({
            "status": "ready",
            "outcome": "completed_with_rejections",
            "current": {"current_rejected_records": 1},
        });
        assert!(source_epoch_findings(&rejections, false, false).is_empty());

        for outcome in [
            "completed_with_source_failures",
            "completed_with_rejections_and_source_failures",
        ] {
            let mut failures = base.clone();
            failures["refresh"] = json!({"status": "partial", "reason": outcome});
            assert_eq!(
                source_epoch_findings(&failures, false, false),
                vec![format!("refresh is partial ({outcome})")],
            );
        }
    }

    #[test]
    fn findings_are_numbered_wrapped_and_actionable() {
        let finding = "ctx Pro key store is unavailable; unlock or repair the already selected secure key store, then run `ctx pro`".to_owned();
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, "off", std::slice::from_ref(&finding), 0);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! ctx found 1 issue\n"));
            assert!(rendered.contains("Issues\n[1]"));
            assert!(rendered.contains("ctx doctor\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn failed_source_findings_are_human_and_recover_through_daemon_status() {
        let findings = [
            "history_epoch is unavailable (source_refresh_failed)",
            "lexical is unavailable (source_refresh_failed)",
            "catalog is pending (catalog_publication_pending)",
            "refresh is unavailable (core_refresh_failed)",
        ]
        .map(str::to_owned);

        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, "apply", &findings, 0);
            let rendered = document.render_plain();
            let flattened = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx found 4 issues\n"));
            for expected in [
                "History is unavailable",
                "Search index is unavailable",
                "History source catalog is still preparing",
                "History refresh is unavailable",
                "Check the history refresh service.",
                "ctx daemon status",
            ] {
                assert!(
                    flattened.contains(expected),
                    "missing {expected:?}: {rendered}"
                );
            }
            for internal in [
                "history_epoch",
                "source_refresh_failed",
                "catalog_publication_pending",
                "lexical_generation_unavailable",
            ] {
                assert!(
                    !rendered.contains(internal),
                    "leaked {internal:?}: {rendered}"
                );
            }
            assert!(!rendered.contains("ctx doctor\n"), "{rendered}");
            assert_fits(&document, &context);
        }
    }
}
