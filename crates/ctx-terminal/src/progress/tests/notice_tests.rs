use super::*;

#[test]
fn trusted_progress_notice_preserves_exact_lines_in_plain_and_json_modes() {
    let lines = [
        "first approved line",
        "second approved line — local",
        "third approved line",
        "fourth approved line → https://companion.example.test/opaque-code",
    ];
    let expected = lines.join("\n");

    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, _) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );
    ProgressReporter::new(&mut ui, ProgressMode::Plain, false, "setup", 0)
        .notice("companion", &lines)
        .unwrap();
    assert_eq!(stderr_capture.text(), format!("{expected}\n"));

    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let (mut ui, _) = ui_with_stderr(
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );
    ProgressReporter::new(&mut ui, ProgressMode::Json, false, "setup", 0)
        .notice("companion", &lines)
        .unwrap();
    let event: serde_json::Value = serde_json::from_str(stderr_capture.text().trim()).unwrap();
    assert_eq!(event["phase"], "companion");
    assert_eq!(event["message"], expected);
}

#[test]
fn live_notice_is_composed_once_into_every_later_refresh_frame() {
    let disclosure = [
        "first trusted companion line",
        "second trusted companion line",
        "third trusted companion line",
        "",
        "trusted action:",
        "https://companion.example.test/opaque-code",
    ];
    let mut notice = Document::new();
    notice.push_blank();
    for line in disclosure {
        notice.push_line(Line::text(line));
    }
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        120,
    ));
    let mut frame_height = None;

    let frozen_histories = Some(vec!["Codex".to_owned(), "Claude".to_owned()]);
    for mut snapshot in [active_status(), active_transfer_status(), terminal_status()] {
        let terminal = snapshot.is_terminal();
        snapshot.set_presentation_agent_histories(frozen_histories.clone());
        let frame = render_live_refresh(
            LiveRefreshPresentation::Setup,
            &context,
            snapshot,
            Some(&notice),
        );
        let rendered = frame.render_plain();
        assert_eq!(rendered.matches(disclosure[0]).count(), 1, "{rendered}");
        assert_eq!(rendered.matches(disclosure[5]).count(), 1, "{rendered}");
        let notice_end = frame.lines().len() - usize::from(terminal);
        assert_eq!(
            &frame.lines()[notice_end - notice.lines().len()..notice_end],
            notice.lines()
        );
        if let Some(frame_height) = frame_height {
            assert_eq!(frame.lines().len(), frame_height);
        } else {
            frame_height = Some(frame.lines().len());
        }
    }
}

#[test]
fn persistent_notice_survives_ten_hz_whole_run_eta_ticks_at_stable_height() {
    let mut notice = Document::new();
    notice.push_blank();
    notice.push_line(Line::text("persistent Pro disclosure"));
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        100,
    ));
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
        let frame = render_live_refresh(
            LiveRefreshPresentation::Setup,
            &context,
            prepared,
            Some(&notice),
        );
        let rendered = frame.render_plain();
        assert_eq!(rendered.matches("persistent Pro disclosure").count(), 1);
        if let Some(frame_height) = frame_height {
            assert_eq!(frame.lines().len(), frame_height);
        } else {
            frame_height = Some(frame.lines().len());
        }
    }
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
        reporter
            .notice(
                "companion",
                &[
                    "first trusted companion line",
                    "second trusted companion line",
                    "third trusted companion line",
                    "",
                    "trusted action:",
                    "https://companion.example.test/opaque-code",
                ],
            )
            .unwrap();
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
    assert!(
        stderr.contains("second trusted companion line"),
        "{stderr:?}"
    );
    assert!(
        stderr.contains("https://companion.example.test/opaque-code"),
        "{stderr:?}"
    );
}
