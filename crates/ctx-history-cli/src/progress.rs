//! Neutral refresh-progress conversion and terminal reporting.

use ctx_history_refresh::{
    RefreshLogicalPhase as EngineLogicalPhase, RefreshRequestState as EngineRequestState,
    RefreshStatus, RefreshStatusKind as EngineStatusKind,
    SourceBackedCurrentSourceProgress as EngineCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage as EngineCurrentSourceProgressStage,
};
use ctx_terminal::{
    RefreshCurrentSourceProgress, RefreshCurrentSourceProgressStage, RefreshLogicalPhase,
    RefreshLogicalStatus, RefreshProgress, RefreshProgressSnapshot, RefreshRequestState,
    RefreshStatusKind, RefreshStructuredOutcome, Ui,
};

pub use ctx_terminal::{format_bytes, format_count, ProgressWriterError};

use crate::ProgressMode;

/// Converts validated engine refresh status into the terminal crate's neutral
/// snapshot before output is rendered.
pub struct ProgressReporter<'a>(ctx_terminal::ProgressReporter<'a>);

impl<'a> ProgressReporter<'a> {
    pub fn new(
        ui: &'a mut Ui,
        mode: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        Self(ctx_terminal::ProgressReporter::new(
            ui,
            match mode {
                ProgressMode::Auto => ctx_terminal::ProgressMode::Auto,
                ProgressMode::Plain => ctx_terminal::ProgressMode::Plain,
                ProgressMode::Json => ctx_terminal::ProgressMode::Json,
                ProgressMode::None => ctx_terminal::ProgressMode::None,
            },
            json_output,
            operation,
            total_bytes,
        ))
    }

    pub fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        self.0.message(phase, message)
    }

    pub fn source_refresh(&mut self, status: &RefreshStatus) -> Result<(), ProgressWriterError> {
        let snapshot = presentation_snapshot(status).map_err(|error| {
            ProgressWriterError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        self.0.source_refresh(snapshot)
    }
}

pub fn presentation_snapshot(status: &RefreshStatus) -> anyhow::Result<RefreshProgressSnapshot> {
    let kind = match status.kind()? {
        EngineStatusKind::Legacy { request_state } => RefreshStatusKind::Legacy {
            request_state: presentation_request_state(request_state),
        },
        EngineStatusKind::BackgroundMaintenanceWake(_) => {
            RefreshStatusKind::BackgroundMaintenanceWake
        }
        EngineStatusKind::Logical(logical) => RefreshStatusKind::Logical(RefreshLogicalStatus {
            request_state: presentation_request_state(logical.request_state),
            logical_phase: presentation_logical_phase(logical.logical_phase),
            physical_attempt_id: logical.physical_attempt_id,
            physical_attempt_state: presentation_request_state(logical.physical_attempt_state),
            progress_owner_request_id: logical.progress_owner_request_id,
            progress_owner_attempt_state: presentation_request_state(
                logical.progress_owner_attempt_state,
            ),
            structured_outcome: logical.structured_outcome.map(|outcome| {
                Box::new(RefreshStructuredOutcome {
                    code: outcome.code.as_str().to_owned(),
                    class: outcome.class.as_str().to_owned(),
                    retryable: outcome.retryable,
                    affected_routes: outcome
                        .affected_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    retryable_routes: outcome
                        .retryable_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    blocked_routes: outcome
                        .blocked_routes
                        .iter()
                        .map(|route| route.as_str().to_owned())
                        .collect(),
                    physical_attempt_id: outcome.physical_attempt_id,
                    retained_generation: outcome.retained_generation,
                    published_generation: outcome.published_generation,
                    retry_advice: outcome
                        .retry_advice
                        .map(|advice| advice.as_str().to_owned()),
                    detail: outcome.detail,
                    failure: outcome.code.is_failure(),
                })
            }),
        }),
    };
    let progress = status.progress()?;
    Ok(RefreshProgressSnapshot::new(
        status.request_id().map(ToOwned::to_owned),
        kind,
        RefreshProgress {
            phase: progress.phase,
            completed_sources: progress.completed_sources as u64,
            total_sources: progress.total_sources as u64,
            current_source: progress.current_source,
            completed_records: progress.completed_records,
            completed_bytes: progress.completed_bytes,
            current_source_progress: progress
                .current_source_progress
                .map(presentation_current_source_progress),
        },
        status.total_sources_known()?,
    ))
}

fn presentation_request_state(value: EngineRequestState) -> RefreshRequestState {
    match value {
        EngineRequestState::AdmissionPending => RefreshRequestState::AdmissionPending,
        EngineRequestState::Queued => RefreshRequestState::Queued,
        EngineRequestState::Running => RefreshRequestState::Running,
        EngineRequestState::Published => RefreshRequestState::Published,
        EngineRequestState::Failed => RefreshRequestState::Failed,
    }
}

fn presentation_logical_phase(value: EngineLogicalPhase) -> RefreshLogicalPhase {
    match value {
        EngineLogicalPhase::Waiting => RefreshLogicalPhase::Waiting,
        EngineLogicalPhase::Attached => RefreshLogicalPhase::Attached,
        EngineLogicalPhase::CoverageCheck => RefreshLogicalPhase::CoverageCheck,
        EngineLogicalPhase::ExactSuccessor => RefreshLogicalPhase::ExactSuccessor,
        EngineLogicalPhase::Direct => RefreshLogicalPhase::Direct,
        EngineLogicalPhase::Terminal => RefreshLogicalPhase::Terminal,
    }
}

fn presentation_current_source_progress(
    value: EngineCurrentSourceProgress,
) -> RefreshCurrentSourceProgress {
    RefreshCurrentSourceProgress {
        stage: match value.stage {
            EngineCurrentSourceProgressStage::SourceFamilyCopy => {
                RefreshCurrentSourceProgressStage::SourceFamilyCopy
            }
            EngineCurrentSourceProgressStage::OnlineBackup => {
                RefreshCurrentSourceProgressStage::OnlineBackup
            }
            EngineCurrentSourceProgressStage::LogicalFingerprint => {
                RefreshCurrentSourceProgressStage::LogicalFingerprint
            }
            EngineCurrentSourceProgressStage::LogicalScan => {
                RefreshCurrentSourceProgressStage::LogicalScan
            }
        },
        snapshot_pages_completed: value.snapshot_pages_completed,
        snapshot_pages_total: value.snapshot_pages_total,
        snapshot_bytes_completed: value.snapshot_bytes_completed,
        snapshot_bytes_total: value.snapshot_bytes_total,
        logical_rows_scanned: value.logical_rows_scanned,
        logical_certified_bytes: value.logical_certified_bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use serde_json::json;

    use super::*;
    use ctx_terminal::{RenderContext, StreamKind, TestContext};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn typed_status(progress: serde_json::Value) -> RefreshStatus {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": "exact_successor",
            "physical_attempt_id": "published-predecessor",
            "physical_attempt_state": "published",
            "progress_owner_request_id": "published-predecessor",
            "progress_owner_attempt_state": "published",
            "progress": progress,
        }))
        .unwrap()
    }

    #[test]
    fn logical_request_state_remains_authoritative_over_published_attempt() {
        let snapshot = presentation_snapshot(&typed_status(json!({
            "phase": "committed",
            "completed_sources": 2,
            "total_sources": 2,
        })))
        .unwrap();

        assert_eq!(
            snapshot.kind().request_state(),
            RefreshRequestState::Running
        );
        assert!(!snapshot.is_terminal());
        assert_eq!(snapshot.phase(), "committed");
    }

    #[test]
    fn legacy_nonzero_total_without_known_field_remains_known() {
        let snapshot = presentation_snapshot(&typed_status(json!({
            "phase": "refreshing",
            "completed_sources": 1,
            "total_sources": 2,
        })))
        .unwrap();

        assert!(snapshot.total_sources_known());
    }

    #[test]
    fn typed_adapter_drops_additive_current_source_progress_fields() {
        let status = typed_status(json!({
            "phase": "copying",
            "completed_sources": 1,
            "total_sources": 2,
            "total_sources_known": true,
            "current_source": "/history.sqlite",
            "completed_records": 8,
            "completed_bytes": 256,
            "current_source_progress": {
                "stage": "online_backup",
                "snapshot_pages_completed": 2,
                "snapshot_pages_total": 4,
                "snapshot_bytes_completed": 256,
                "snapshot_bytes_total": 512,
                "future_additive_field": "must-not-leak"
            }
        }));
        let stdout = SharedWriter::default();
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let mut ui = Ui::with_writers(
            stdout,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );

        ProgressReporter::new(&mut ui, ProgressMode::Json, false, "import", 0)
            .source_refresh(&status)
            .unwrap();

        let event: serde_json::Value = serde_json::from_str(stderr_capture.text().trim()).unwrap();
        assert_eq!(
            event["current_source_progress"],
            json!({
                "stage": "online_backup",
                "snapshot_pages_completed": 2,
                "snapshot_pages_total": 4,
                "snapshot_bytes_completed": 256,
                "snapshot_bytes_total": 512,
            })
        );
    }
}
