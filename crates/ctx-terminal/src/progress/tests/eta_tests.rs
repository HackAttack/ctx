use super::*;

#[test]
fn local_live_worker_ticks_without_another_backend_callback() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        80,
    ));
    let (mut ui, _) = ui_with_stderr(stderr, context);
    let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Auto, false, "setup", 0);
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(1_000);

    reporter
        .source_refresh_at(snapshot, StdDuration::from_secs(1))
        .unwrap();
    let first_len = stderr_capture.text().len();
    let deadline = Instant::now() + StdDuration::from_secs(1);
    while stderr_capture.text().len() == first_len && Instant::now() < deadline {
        thread::sleep(StdDuration::from_millis(10));
    }
    let output = stderr_capture.text();
    let delta = &output[first_len..];

    assert!(output.contains("Elapsed              1s"), "{output:?}");
    assert!(!delta.is_empty());
    assert!(!delta.contains("\x1b[2J"), "{delta:?}");
    assert!(!delta.contains("\x1b[H"), "{delta:?}");
    assert!(!delta.contains("\x1b[2K"), "{delta:?}");
}

#[test]
fn local_elapsed_clock_never_regresses_when_backend_snapshots_are_stale() {
    let mut clock = ActiveElapsedClock::default();
    assert_eq!(
        clock.advance(Some(10_000), StdDuration::from_secs(10), true),
        10_000
    );
    assert_eq!(
        clock.advance(Some(9_000), StdDuration::from_millis(10_100), true),
        10_100
    );
    assert_eq!(
        clock.advance(Some(12_000), StdDuration::from_millis(10_200), true),
        12_000
    );
}

#[test]
fn local_live_clock_counts_down_whole_run_eta_without_backend_updates() {
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(5_000);
    let mut clock = ActiveElapsedClock::default();

    let first = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(10),
        true,
    );
    assert_eq!(first.estimated_remaining_millis(), Some(5_000));

    let next = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_millis(10_100),
        false,
    );
    assert_eq!(next.estimated_remaining_millis(), Some(4_900));

    let expired = prepare_live_snapshot(snapshot, &mut clock, StdDuration::from_secs(15), false);
    assert_eq!(expired.estimated_remaining_millis(), None);
}

#[test]
fn local_live_clock_suppresses_eta_after_backend_snapshot_silence() {
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(30_000);
    let mut clock = ActiveElapsedClock::default();

    let fresh = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(10),
        true,
    );
    assert_eq!(fresh.estimated_remaining_millis(), Some(30_000));

    let nearly_stale = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_millis(14_999),
        false,
    );
    assert_eq!(nearly_stale.estimated_remaining_millis(), Some(25_001));

    let stale = prepare_live_snapshot(snapshot, &mut clock, StdDuration::from_secs(15), false);
    assert_eq!(stale.estimated_remaining_millis(), None);

    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        80,
    ));
    let rendered =
        render_live_refresh(LiveRefreshPresentation::Setup, &context, stale, None).render_plain();
    assert!(
        rendered.contains("Estimated remaining  Estimating"),
        "{rendered}"
    );
    assert!(
        !rendered.lines().nth(1).unwrap_or_default().contains('%'),
        "{rendered}"
    );
}

#[test]
fn identical_backend_snapshots_do_not_extend_the_silence_deadline() {
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(30_000);
    let mut clock = ActiveElapsedClock::default();

    let fresh = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(10),
        true,
    );
    assert_eq!(fresh.estimated_remaining_millis(), Some(30_000));

    let repeated = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(12),
        true,
    );
    assert_eq!(repeated.estimated_remaining_millis(), Some(28_000));

    let nearly_stale = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_millis(14_999),
        true,
    );
    assert_eq!(nearly_stale.estimated_remaining_millis(), Some(25_001));

    let stale = prepare_live_snapshot(snapshot, &mut clock, StdDuration::from_millis(15_001), true);
    assert_eq!(stale.estimated_remaining_millis(), None);
}

#[test]
fn regressed_backend_snapshot_does_not_extend_the_silence_deadline() {
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(30_000);
    let mut clock = ActiveElapsedClock::default();

    prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(10),
        true,
    );

    let mut regressed = snapshot;
    regressed.progress_mut_for_test().elapsed_millis = Some(9_000);
    let still_live = prepare_live_snapshot(
        regressed.clone(),
        &mut clock,
        StdDuration::from_secs(14),
        true,
    );
    assert_eq!(still_live.estimated_remaining_millis(), Some(25_000));

    let stale = prepare_live_snapshot(
        regressed,
        &mut clock,
        StdDuration::from_millis(15_001),
        false,
    );
    assert_eq!(stale.estimated_remaining_millis(), None);
}

#[test]
fn advanced_backend_snapshot_restores_liveness_after_silence() {
    let mut snapshot = active_status();
    snapshot.progress_mut_for_test().elapsed_millis = Some(10_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(30_000);
    let mut clock = ActiveElapsedClock::default();

    prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(10),
        true,
    );
    let stale = prepare_live_snapshot(
        snapshot.clone(),
        &mut clock,
        StdDuration::from_secs(15),
        false,
    );
    assert_eq!(stale.estimated_remaining_millis(), None);

    snapshot.progress_mut_for_test().elapsed_millis = Some(16_000);
    snapshot.progress_mut_for_test().estimated_remaining_millis = Some(20_000);
    let resumed = prepare_live_snapshot(snapshot, &mut clock, StdDuration::from_secs(16), true);
    assert_eq!(resumed.estimated_remaining_millis(), Some(20_000));
}
