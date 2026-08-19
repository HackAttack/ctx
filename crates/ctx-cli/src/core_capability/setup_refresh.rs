use super::*;

pub(super) fn core_setup_refresh(
    data_root: &Path,
    wait: bool,
    defer_fresh_empty_wait: bool,
    notice_lines: &[String],
    progress_mode: SetupProgressMode,
    events: &mut dyn CapabilityEventSink,
) -> Result<(Option<String>, Value)> {
    match progress_mode {
        SetupProgressMode::Legacy(mode) => {
            core_setup_refresh_legacy(data_root, wait, defer_fresh_empty_wait, notice_lines, mode)
        }
        SetupProgressMode::Events => {
            core_setup_refresh_events(data_root, wait, defer_fresh_empty_wait, events)
        }
    }
}

fn core_setup_refresh_events(
    data_root: &Path,
    wait: bool,
    defer_fresh_empty_wait: bool,
    events: &mut dyn CapabilityEventSink,
) -> Result<(Option<String>, Value)> {
    let mut effective_wait = wait;
    let mut terminal_progress = None;
    let result = {
        let mut progress = |status: &crate::semantic::RefreshStatus| {
            if status.kind()?.request_state().is_terminal() {
                terminal_progress = Some(status.schema_v1_fields().clone());
                Ok(())
            } else {
                events.refresh(status)
            }
        };
        let mode = if wait {
            crate::semantic::SourceBackedRefreshMode::Wait
        } else {
            crate::semantic::SourceBackedRefreshMode::Background
        };
        let mut result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
            data_root,
            mode,
            &mut progress,
        );
        if result.as_ref().is_err_and(|error| {
            !defer_fresh_empty_wait && should_wait_for_fresh_empty_publication(wait, error)
        }) {
            effective_wait = true;
            result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
                data_root,
                crate::semantic::SourceBackedRefreshMode::Wait,
                &mut progress,
            );
        }
        result
    };
    match result {
        Ok(observation) => {
            if let Some(terminal) = terminal_progress.take() {
                let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
                events.refresh(&terminal)?;
            }
            let generation_id = observation.pin.generation_id().to_owned();
            let receipt = observation
                .receipt
                .as_ref()
                .map(|receipt| receipt.to_json());
            Ok((
                Some(generation_id.clone()),
                json!({
                    "daemon_available": observation.daemon_available,
                    "mode": if effective_wait { "wait" } else { "background" },
                    "published_generation": generation_id,
                    "reason": Value::Null,
                    "receipt": receipt,
                    "request_id": observation.request_id,
                    "source_count": observation.source_count,
                    "status": observation.status,
                }),
            ))
        }
        Err(error) => {
            if let Some(terminal) = terminal_progress.take() {
                let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
                events.refresh(&terminal)?;
            }
            // A caller that explicitly waited asked for a usable generation, not
            // merely admission. Keep background admission fail-soft, but never
            // turn a failed waited refresh into a successful setup response.
            if should_propagate_setup_refresh_failure(effective_wait, &error) {
                return Err(error);
            }
            let pending = (!effective_wait)
                .then(|| {
                    error.downcast_ref::<crate::semantic::SourceBackedRefreshPendingPublication>()
                })
                .flatten();
            Ok((
                None,
                json!({
                    "daemon_available": true,
                    "last_error": format!("{error:#}"),
                    "mode": if effective_wait { "wait" } else { "background" },
                    "reason": if pending.is_some() {
                        "refresh_queued_without_published_generation"
                    } else {
                        "refresh_failed"
                    },
                    "request_id": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_id),
                    "request_state": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_state),
                    "source_count": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::source_count),
                    "status": if pending.is_some() { "pending" } else { "unavailable" },
                }),
            ))
        }
    }
}

fn core_setup_refresh_legacy(
    data_root: &Path,
    wait: bool,
    defer_fresh_empty_wait: bool,
    notice_lines: &[String],
    progress_mode: crate::progress::ProgressArg,
) -> Result<(Option<String>, Value)> {
    let mut effective_wait = wait;
    let mut ui = crate::ui::Ui::stdio(ctx_terminal::ui::ColorMode::Auto);
    let progress_mode = progress_mode_for_notice(
        progress_mode,
        ui.stderr_context().content_width(),
        notice_lines,
    );
    let mut reporter =
        crate::progress::ProgressReporter::new(&mut ui, progress_mode.into(), false, "setup", 0);
    if !notice_lines.is_empty() {
        let lines = notice_lines.iter().map(String::as_str).collect::<Vec<_>>();
        reporter.notice("companion", &lines)?;
    }
    let defer_terminal = reporter.is_enabled();
    let mut terminal_progress = None;
    let result = {
        let mut progress = |status: &crate::semantic::RefreshStatus| {
            if defer_terminal && status.kind()?.request_state().is_terminal() {
                terminal_progress = Some(status.schema_v1_fields().clone());
                Ok(())
            } else {
                reporter.source_refresh(status).map_err(anyhow::Error::new)
            }
        };
        let mode = if wait {
            crate::semantic::SourceBackedRefreshMode::Wait
        } else {
            crate::semantic::SourceBackedRefreshMode::Background
        };
        let mut result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
            data_root,
            mode,
            &mut progress,
        );
        if result.as_ref().is_err_and(|error| {
            !defer_fresh_empty_wait && should_wait_for_fresh_empty_publication(wait, error)
        }) {
            effective_wait = true;
            result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
                data_root,
                crate::semantic::SourceBackedRefreshMode::Wait,
                &mut progress,
            );
        }
        result
    };
    match result {
        Ok(observation) => {
            if let Some(terminal) = terminal_progress.take() {
                let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
                reporter.source_refresh_with_published_index(
                    &terminal,
                    observation.pin.verified_index(),
                )?;
            }
            let generation_id = observation.pin.generation_id().to_owned();
            let receipt = observation
                .receipt
                .as_ref()
                .map(|receipt| receipt.to_json());
            Ok((
                Some(generation_id.clone()),
                json!({
                    "daemon_available": observation.daemon_available,
                    "mode": if effective_wait { "wait" } else { "background" },
                    "published_generation": generation_id,
                    "reason": Value::Null,
                    "receipt": receipt,
                    "request_id": observation.request_id,
                    "source_count": observation.source_count,
                    "status": observation.status,
                }),
            ))
        }
        Err(error) => {
            if let Some(terminal) = terminal_progress.take() {
                let terminal = crate::semantic::RefreshStatus::parse_schema_v1(terminal)?;
                reporter
                    .source_refresh(&terminal)
                    .map_err(anyhow::Error::new)?;
            }
            if should_propagate_setup_refresh_failure(effective_wait, &error) {
                return Err(error);
            }
            let pending = (!effective_wait)
                .then(|| {
                    error.downcast_ref::<crate::semantic::SourceBackedRefreshPendingPublication>()
                })
                .flatten();
            Ok((
                None,
                json!({
                    "daemon_available": true,
                    "last_error": format!("{error:#}"),
                    "mode": if effective_wait { "wait" } else { "background" },
                    "reason": if pending.is_some() {
                        "refresh_queued_without_published_generation"
                    } else {
                        "refresh_failed"
                    },
                    "request_id": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_id),
                    "request_state": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_state),
                    "source_count": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::source_count),
                    "status": if pending.is_some() { "pending" } else { "unavailable" },
                }),
            ))
        }
    }
}

pub(super) fn should_propagate_setup_refresh_failure(
    effective_wait: bool,
    error: &anyhow::Error,
) -> bool {
    effective_wait
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<crate::progress::ProgressWriterError>()
                .is_some()
        })
        || progress_events::event_writer_error(error)
}

pub(super) fn should_wait_for_fresh_empty_publication(wait: bool, error: &anyhow::Error) -> bool {
    !wait
        && error
            .downcast_ref::<crate::semantic::SourceBackedRefreshPendingPublication>()
            .is_some_and(|pending| pending.source_count() == 0)
}
