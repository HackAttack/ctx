use super::*;

fn presentation() -> CalloutPresentation {
    CalloutPresentation::new(
        "A companion",
        vec![
            CalloutRow::Bullet("A static local product fact.".to_owned()),
            CalloutRow::Blank,
            CalloutRow::Status {
                level: CalloutStatus::Success,
                text: "Access is ready.".to_owned(),
            },
            CalloutRow::Blank,
            CalloutRow::Action("Continue here:".to_owned()),
            CalloutRow::Reference("https://companion.example.test/aB3dE7fGh9Jk".to_owned()),
        ],
    )
}

#[test]
fn structured_callout_is_ordered_after_plain_progress_and_stays_structured_in_json() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, _) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );
    let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Plain, false, "setup", 0);
    reporter.callout("companion", presentation()).unwrap();
    assert!(stderr_capture.text().is_empty());
    reporter.source_refresh(terminal_status()).unwrap();
    let plain = stderr_capture.text();
    assert!(
        plain.find("History refresh complete").unwrap() < plain.find("A companion").unwrap(),
        "{plain}"
    );
    assert_eq!(plain.matches("A companion").count(), 1, "{plain}");
    assert_eq!(plain.matches("aB3dE7fGh9Jk").count(), 1, "{plain}");

    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, _) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );
    ProgressReporter::new(&mut ui, ProgressMode::Json, false, "setup", 0)
        .callout("companion", presentation())
        .unwrap();
    let event: serde_json::Value = serde_json::from_str(stderr_capture.text().trim()).unwrap();
    assert_eq!(event["phase"], "companion");
    assert_eq!(event["callout"]["title"], "A companion");
    assert_eq!(event["callout"]["rows"][2]["kind"], "status");
    assert_eq!(event["callout"]["rows"][5]["kind"], "reference");
    assert!(!stderr_capture.text().contains('\u{1b}'));
}

#[test]
fn structured_callout_json_contains_no_raw_c0_c1_or_del_controls() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, _) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );
    let presentation = CalloutPresentation::new(
        "title\u{1b}\u{009b}",
        vec![CalloutRow::Status {
            level: CalloutStatus::Warning,
            text: "status\0\r\u{007f}\u{0085}".to_owned(),
        }],
    );

    ProgressReporter::new(&mut ui, ProgressMode::Json, false, "setup", 0)
        .callout("companion", presentation)
        .unwrap();

    let output = stderr_capture.text();
    assert!(
        !output.trim_end().chars().any(char::is_control),
        "{output:?}"
    );
    let event: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(event["callout"]["title"], "title\\x1b\\u{009b}");
    assert_eq!(
        event["callout"]["rows"][0]["text"],
        "status\\u{0000}\\r\\u{007f}\\u{0085}"
    );
}

#[test]
fn live_callout_is_composed_once_and_terminal_frames_use_natural_height() {
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        120,
    ));
    let callout = presentation();
    let frozen_histories = Some(vec!["Codex".to_owned(), "Claude".to_owned()]);
    let mut heights = Vec::new();

    for mut snapshot in [active_status(), active_transfer_status(), terminal_status()] {
        snapshot.set_presentation_agent_histories(frozen_histories.clone());
        let progress_label = if snapshot.is_terminal() {
            "History refresh complete"
        } else {
            "Reading your agent history"
        };
        let frame = render_live_refresh(
            LiveRefreshPresentation::Setup,
            &context,
            snapshot,
            None,
            Some(&callout),
        );
        let rendered = frame.render_plain();
        assert_eq!(rendered.matches("A companion").count(), 1, "{rendered}");
        assert_eq!(rendered.matches("aB3dE7fGh9Jk").count(), 1, "{rendered}");
        assert!(
            rendered.find(progress_label).unwrap() < rendered.find("A companion").unwrap(),
            "{rendered}"
        );
        heights.push(frame.lines().len());
    }
    assert_eq!(heights[0], heights[1]);
    assert!(heights[2] < heights[1], "{heights:?}");
}

#[test]
fn persistent_callout_survives_ten_hz_ticks_and_reflows_from_narrow_to_wide() {
    let callout = presentation();
    let mut snapshot = active_status();
    snapshot.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(5_000);
    let mut clock = ActiveElapsedClock::default();
    let mut frame_height = None;

    for (now, backend_snapshot, expected_remaining) in [
        (StdDuration::from_secs(10), true, 5_000),
        (StdDuration::from_millis(10_100), false, 4_900),
        (StdDuration::from_millis(10_200), false, 4_800),
    ] {
        let prepared = prepare_live_snapshot(snapshot.clone(), &mut clock, now, backend_snapshot);
        assert_eq!(
            prepared.estimated_remaining_millis(),
            Some(expected_remaining)
        );
        let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
            crate::ui::StreamKind::Stderr,
            80,
        ));
        let frame = render_live_refresh(
            LiveRefreshPresentation::Setup,
            &context,
            prepared,
            None,
            Some(&callout),
        );
        assert_eq!(frame.render_plain().matches("A companion").count(), 1);
        if let Some(frame_height) = frame_height {
            assert_eq!(frame.lines().len(), frame_height);
        } else {
            frame_height = Some(frame.lines().len());
        }
    }

    let narrow = callout
        .render(&crate::ui::RenderContext::for_test(
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 32),
        ))
        .render_plain();
    let wide = callout
        .render(&crate::ui::RenderContext::for_test(
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 80),
        ))
        .render_plain();
    assert!(!narrow.contains('│'), "{narrow}");
    assert!(wide.contains('│'), "{wide}");
    assert_eq!(narrow.matches("aB3dE7fGh9Jk").count(), 1);
    assert_eq!(wide.matches("aB3dE7fGh9Jk").count(), 1);
}

#[test]
fn hosted_json_live_progress_keeps_stdout_machine_clean() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, stdout_capture) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
            crate::ui::StreamKind::Stderr,
            120,
        )),
    );
    {
        let mut reporter = ProgressReporter::new_with_live_json_stderr(
            &mut ui,
            ProgressMode::Auto,
            true,
            "setup",
            0,
            true,
        );
        reporter.callout("companion", presentation()).unwrap();
        reporter.source_refresh(active_status()).unwrap();
        reporter.source_refresh(active_transfer_status()).unwrap();
    }
    assert!(stdout_capture.text().is_empty());
    ui.write_stdout(&Document::from_line(Line::text(
        r#"{"payload_type":"setup"}"#,
    )))
    .unwrap();
    assert_eq!(stdout_capture.text(), "{\"payload_type\":\"setup\"}\n");
    let stderr = stderr_capture.text();
    assert!(stderr.contains("Reading your agent history"), "{stderr:?}");
    assert_eq!(stderr.matches("A companion").count(), 1, "{stderr:?}");
    assert!(stderr.contains("aB3dE7fGh9Jk"), "{stderr:?}");
}
