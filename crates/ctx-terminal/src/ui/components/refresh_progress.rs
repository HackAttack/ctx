use serde_json::{json, Value};

use crate::{format_bytes, format_count};

use super::{fields, progress, Field, Progress};
use crate::ui::{Document, RenderContext};

const MAX_DYNAMIC_TEXT_BYTES: usize = 256;

/// Terminal-neutral presentation view of a refresh status. Composition code
/// converts its domain snapshot to this owned value before rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshProgressSnapshot {
    request_id: Option<String>,
    kind: RefreshStatusKind,
    progress: RefreshProgress,
    total_sources_known: bool,
}

impl RefreshProgressSnapshot {
    pub fn new(
        request_id: Option<String>,
        kind: RefreshStatusKind,
        progress: RefreshProgress,
        total_sources_known: bool,
    ) -> Self {
        Self {
            request_id,
            kind,
            progress,
            total_sources_known,
        }
    }

    pub const fn kind(&self) -> &RefreshStatusKind {
        &self.kind
    }

    pub const fn progress(&self) -> &RefreshProgress {
        &self.progress
    }

    pub const fn total_sources_known(&self) -> bool {
        self.total_sources_known
    }

    pub fn is_terminal(&self) -> bool {
        self.kind.request_state().is_terminal()
    }

    pub fn phase(&self) -> String {
        if self.is_terminal() {
            return match self.kind.request_state() {
                RefreshRequestState::Published => "published".to_owned(),
                RefreshRequestState::Failed => "failed".to_owned(),
                RefreshRequestState::AdmissionPending
                | RefreshRequestState::Queued
                | RefreshRequestState::Running => unreachable!("terminal status is not active"),
            };
        }
        self.progress
            .current_source_progress
            .as_ref()
            .map(|current| current.stage.as_str().to_owned())
            .unwrap_or_else(|| self.progress.phase.clone())
    }

    pub fn message(&self) -> String {
        let label = refresh_label(self);
        let sources = source_count_text(self);
        match self.progress.current_source.as_deref() {
            Some(source) if !self.is_terminal() => {
                format!("{label}: {} ({sources}).", bounded_dynamic_text(source))
            }
            _ => format!("{label} ({sources})."),
        }
    }

    pub fn byte_progress(&self) -> (u64, u64) {
        let Some(current) = self.progress.current_source_progress.as_ref() else {
            return (0, 0);
        };
        match current.stage {
            RefreshCurrentSourceProgressStage::SourceFamilyCopy
            | RefreshCurrentSourceProgressStage::OnlineBackup => current
                .snapshot_bytes_completed
                .zip(current.snapshot_bytes_total)
                .unwrap_or((0, 0)),
            RefreshCurrentSourceProgressStage::LogicalFingerprint
            | RefreshCurrentSourceProgressStage::LogicalScan => (0, 0),
        }
    }

    pub(crate) fn append_json_fields(&self, value: &mut Value) {
        if let Some(request_id) = self.request_id.as_ref() {
            value["request_id"] = json!(request_id);
        }
        value["request_state"] = json!(self.kind.request_state().as_str());
        match &self.kind {
            RefreshStatusKind::Legacy { .. } => {}
            RefreshStatusKind::BackgroundMaintenanceWake => {
                if let Some(request_id) = self.request_id.as_ref() {
                    value["logical_request_id"] = json!(request_id);
                }
                value["logical_phase"] = json!(RefreshLogicalPhase::Waiting.as_str());
                value["maintenance_wake"] = json!(true);
            }
            RefreshStatusKind::Logical(logical) => {
                if let Some(request_id) = self.request_id.as_ref() {
                    value["logical_request_id"] = json!(request_id);
                }
                value["logical_phase"] = json!(logical.logical_phase.as_str());
                value["physical_attempt_id"] = json!(logical.physical_attempt_id);
                value["physical_attempt_state"] = json!(logical.physical_attempt_state.as_str());
                value["progress_owner_request_id"] = json!(logical.progress_owner_request_id);
                value["progress_owner_attempt_state"] =
                    json!(logical.progress_owner_attempt_state.as_str());
                if let Some(outcome) = logical.structured_outcome.as_ref() {
                    value["structured_outcome"] = outcome.to_json();
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRequestState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}
impl RefreshRequestState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Failed)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionPending => "admission_pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshLogicalPhase {
    Waiting,
    Attached,
    CoverageCheck,
    ExactSuccessor,
    Direct,
    Terminal,
}
impl RefreshLogicalPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Attached => "attached",
            Self::CoverageCheck => "coverage_check",
            Self::ExactSuccessor => "exact_successor",
            Self::Direct => "direct",
            Self::Terminal => "terminal",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshStatusKind {
    Legacy { request_state: RefreshRequestState },
    BackgroundMaintenanceWake,
    Logical(RefreshLogicalStatus),
}
impl RefreshStatusKind {
    pub const fn request_state(&self) -> RefreshRequestState {
        match self {
            Self::Legacy { request_state } => *request_state,
            Self::BackgroundMaintenanceWake => RefreshRequestState::Queued,
            Self::Logical(logical) => logical.request_state,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshLogicalStatus {
    pub request_state: RefreshRequestState,
    pub logical_phase: RefreshLogicalPhase,
    pub physical_attempt_id: String,
    pub physical_attempt_state: RefreshRequestState,
    pub progress_owner_request_id: String,
    pub progress_owner_attempt_state: RefreshRequestState,
    pub structured_outcome: Option<Box<RefreshStructuredOutcome>>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStructuredOutcome {
    pub code: String,
    pub class: String,
    pub retryable: bool,
    pub affected_routes: Vec<String>,
    pub retryable_routes: Vec<String>,
    pub blocked_routes: Vec<String>,
    pub physical_attempt_id: String,
    pub retained_generation: Option<String>,
    pub published_generation: Option<String>,
    pub retry_advice: Option<String>,
    pub detail: Option<String>,
    pub failure: bool,
}
impl RefreshStructuredOutcome {
    fn is_failure(&self) -> bool {
        self.failure
    }

    fn to_json(&self) -> Value {
        let mut value = json!({
            "code": self.code,
            "class": self.class,
            "retryable": self.retryable,
            "affected_routes": self.affected_routes,
            "retryable_routes": self.retryable_routes,
            "blocked_routes": self.blocked_routes,
            "physical_attempt_id": self.physical_attempt_id,
            "retained_generation": self.retained_generation,
            "published_generation": self.published_generation,
            "retry_advice": self.retry_advice,
            "detail": self.detail,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, field| !field.is_null());
        }
        value
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshProgress {
    pub phase: String,
    pub completed_sources: u64,
    pub total_sources: u64,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub current_source_progress: Option<RefreshCurrentSourceProgress>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
}
impl RefreshCurrentSourceProgressStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshCurrentSourceProgress {
    pub stage: RefreshCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl RefreshCurrentSourceProgress {
    pub(crate) fn to_json(&self) -> Value {
        let mut value = json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, field| !field.is_null());
        }
        value
    }
}

pub fn refresh_progress(context: &RenderContext, snapshot: &RefreshProgressSnapshot) -> Document {
    let completed = snapshot.progress.completed_sources;
    let total = snapshot
        .total_sources_known
        .then_some(snapshot.progress.total_sources)
        .map(|total| total.max(completed));
    let label = refresh_label(snapshot);
    let mut document = progress(
        context,
        Progress {
            label,
            current: completed,
            total,
            detail: None,
        },
    );

    let mut details = vec![
        ("Sources", source_count_text(snapshot)),
        (
            "Logical phase",
            logical_phase_text(&snapshot.kind).to_owned(),
        ),
        (
            "Physical phase",
            humanize(&bounded_dynamic_text(&snapshot.progress.phase)),
        ),
    ];
    if let Some(source) = snapshot.progress.current_source.as_deref() {
        details.push(("Source", bounded_dynamic_text(source)));
    }
    if let Some(records) = snapshot.progress.completed_records {
        details.push(("Records", format!("{} accepted", format_count_u64(records))));
    }
    if let Some(bytes) = snapshot.progress.completed_bytes {
        details.push(("Scanned", format_bytes(bytes)));
    }
    if let RefreshStatusKind::Logical(logical) = &snapshot.kind {
        if let Some(request_id) = snapshot.request_id.as_deref() {
            details.push(("Logical request", bounded_dynamic_text(request_id)));
        }
        details.push((
            "Physical attempt",
            bounded_dynamic_text(&logical.physical_attempt_id),
        ));
        details.push((
            "Physical state",
            request_state_text(logical.physical_attempt_state).to_owned(),
        ));
        details.push((
            "Progress owner",
            bounded_dynamic_text(&logical.progress_owner_request_id),
        ));
        details.push((
            "Owner state",
            request_state_text(logical.progress_owner_attempt_state).to_owned(),
        ));
        if let Some(outcome) = logical.structured_outcome.as_ref() {
            details.push(("Outcome", outcome.code.as_str().replace('_', " ")));
        }
    }

    let detail_fields = details
        .iter()
        .map(|(label, value)| Field::new(label, value))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(fields(context, &detail_fields));
    document
}

fn refresh_label(snapshot: &RefreshProgressSnapshot) -> &'static str {
    match &snapshot.kind {
        RefreshStatusKind::BackgroundMaintenanceWake => "History refresh is queued",
        RefreshStatusKind::Legacy { request_state } => match request_state {
            RefreshRequestState::AdmissionPending | RefreshRequestState::Queued => {
                "History refresh is queued"
            }
            RefreshRequestState::Running => physical_label(&snapshot.progress.phase),
            RefreshRequestState::Published => "History refresh complete",
            RefreshRequestState::Failed => "History refresh failed",
        },
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting => "History refresh is waiting",
            RefreshLogicalPhase::Attached => "Refreshing history with shared work",
            RefreshLogicalPhase::CoverageCheck => "Checking refresh coverage",
            RefreshLogicalPhase::ExactSuccessor => "Waiting for successor refresh",
            RefreshLogicalPhase::Direct => physical_label(&snapshot.progress.phase),
            RefreshLogicalPhase::Terminal => logical
                .structured_outcome
                .as_ref()
                .map(|outcome| {
                    if outcome.is_failure() {
                        "History refresh failed"
                    } else if outcome.code.as_str() == "completed" {
                        "History refresh complete"
                    } else {
                        "History refresh complete with issues"
                    }
                })
                .unwrap_or("History refresh complete"),
        },
    }
}

fn physical_label(phase: &str) -> &'static str {
    match phase {
        "queued" | "pending" | "discovering" => "Discovering history sources",
        "committing" | "committed" | "publishing" => "Publishing search index",
        "verifying" => "Verifying refreshed history",
        _ => "Refreshing history",
    }
}

fn source_count_text(snapshot: &RefreshProgressSnapshot) -> String {
    if snapshot.total_sources_known {
        format!(
            "{} / {}",
            format_count_u64(snapshot.progress.completed_sources),
            format_count_u64(
                snapshot
                    .progress
                    .total_sources
                    .max(snapshot.progress.completed_sources)
            )
        )
    } else {
        "measuring".to_owned()
    }
}

fn logical_phase_text(kind: &RefreshStatusKind) -> &'static str {
    match kind {
        RefreshStatusKind::Legacy { .. } => "legacy",
        RefreshStatusKind::BackgroundMaintenanceWake => "waiting",
        RefreshStatusKind::Logical(logical) => match logical.logical_phase {
            RefreshLogicalPhase::Waiting => "waiting",
            RefreshLogicalPhase::Attached => "attached",
            RefreshLogicalPhase::CoverageCheck => "coverage check",
            RefreshLogicalPhase::ExactSuccessor => "exact successor",
            RefreshLogicalPhase::Direct => "direct",
            RefreshLogicalPhase::Terminal => "terminal",
        },
    }
}

fn request_state_text(state: RefreshRequestState) -> &'static str {
    match state {
        RefreshRequestState::AdmissionPending => "admission pending",
        RefreshRequestState::Queued => "queued",
        RefreshRequestState::Running => "running",
        RefreshRequestState::Published => "published",
        RefreshRequestState::Failed => "failed",
    }
}

fn humanize(value: &str) -> String {
    value.replace('_', " ")
}

fn bounded_dynamic_text(value: &str) -> String {
    if value.len() <= MAX_DYNAMIC_TEXT_BYTES {
        return value.to_owned();
    }
    const SUFFIX: &str = "...";
    let mut end = MAX_DYNAMIC_TEXT_BYTES
        .saturating_sub(SUFFIX.len())
        .min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], SUFFIX)
}

fn format_count_u64(value: u64) -> String {
    usize::try_from(value)
        .map(format_count)
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{StreamKind, TestContext};

    fn active_status(
        logical_phase: RefreshLogicalPhase,
        physical_phase: &str,
        known: bool,
        total: u64,
    ) -> RefreshProgressSnapshot {
        RefreshProgressSnapshot::new(
            Some("logical-request".to_owned()),
            RefreshStatusKind::Logical(RefreshLogicalStatus {
                request_state: RefreshRequestState::Running,
                logical_phase,
                physical_attempt_id: "physical-attempt".to_owned(),
                physical_attempt_state: RefreshRequestState::Running,
                progress_owner_request_id: "progress-owner".to_owned(),
                progress_owner_attempt_state: RefreshRequestState::Running,
                structured_outcome: None,
            }),
            RefreshProgress {
                phase: physical_phase.to_owned(),
                completed_sources: 0,
                total_sources: total,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                current_source_progress: None,
            },
            known,
        )
    }

    fn terminal_status(
        state: RefreshRequestState,
        code: &str,
        class: &str,
        failure: bool,
    ) -> RefreshProgressSnapshot {
        RefreshProgressSnapshot::new(
            Some("logical-request".to_owned()),
            RefreshStatusKind::Logical(RefreshLogicalStatus {
                request_state: state,
                logical_phase: RefreshLogicalPhase::Terminal,
                physical_attempt_id: "physical-attempt".to_owned(),
                physical_attempt_state: state,
                progress_owner_request_id: "physical-attempt".to_owned(),
                progress_owner_attempt_state: state,
                structured_outcome: Some(Box::new(RefreshStructuredOutcome {
                    code: code.to_owned(),
                    class: class.to_owned(),
                    retryable: false,
                    affected_routes: Vec::new(),
                    retryable_routes: Vec::new(),
                    blocked_routes: Vec::new(),
                    physical_attempt_id: "physical-attempt".to_owned(),
                    retained_generation: None,
                    published_generation: None,
                    retry_advice: None,
                    detail: None,
                    failure,
                })),
            }),
            RefreshProgress {
                phase: "committed".to_owned(),
                completed_sources: 0,
                total_sources: 0,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                current_source_progress: None,
            },
            true,
        )
    }

    #[test]
    fn full_status_adapter_preserves_logical_phases_and_physical_owner() {
        for (phase, expected) in [
            (RefreshLogicalPhase::Waiting, "History refresh is waiting"),
            (
                RefreshLogicalPhase::Attached,
                "Refreshing history with shared work",
            ),
            (
                RefreshLogicalPhase::CoverageCheck,
                "Checking refresh coverage",
            ),
            (
                RefreshLogicalPhase::ExactSuccessor,
                "Waiting for successor refresh",
            ),
        ] {
            let snapshot = active_status(phase, "committed", true, 2);
            assert_eq!(refresh_label(&snapshot), expected);
            assert!(!snapshot.is_terminal(), "logical phase {phase:?}");
            let logical = match snapshot.kind() {
                RefreshStatusKind::Logical(logical) => logical,
                other => panic!("unexpected status kind: {other:?}"),
            };
            assert_eq!(logical.physical_attempt_id, "physical-attempt");
            assert_eq!(logical.progress_owner_request_id, "progress-owner");
        }
    }

    #[test]
    fn known_zero_and_unknown_totals_remain_distinct() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let known = active_status(RefreshLogicalPhase::Direct, "discovering", true, 0);
        let unknown = active_status(RefreshLogicalPhase::Direct, "discovering", false, 0);
        assert!(refresh_progress(&context, &known)
            .render_plain()
            .contains("0 / 0"));
        assert!(refresh_progress(&context, &unknown)
            .render_plain()
            .contains("measuring"));
    }

    #[test]
    fn byte_progress_requires_one_complete_engine_snapshot_pair() {
        let mut paired = active_status(RefreshLogicalPhase::Attached, "copying", true, 2);
        paired.progress.current_source_progress = Some(RefreshCurrentSourceProgress {
            stage: RefreshCurrentSourceProgressStage::SourceFamilyCopy,
            snapshot_pages_completed: None,
            snapshot_pages_total: None,
            snapshot_bytes_completed: Some(256),
            snapshot_bytes_total: Some(512),
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        });
        assert_eq!(paired.byte_progress(), (256, 512));

        paired
            .progress
            .current_source_progress
            .as_mut()
            .unwrap()
            .snapshot_bytes_total = None;
        assert_eq!(paired.byte_progress(), (0, 0));
    }

    #[test]
    fn structured_terminal_outcome_alone_decides_done() {
        let cases = [
            (
                RefreshRequestState::Published,
                "completed",
                "completed",
                false,
                "History refresh complete",
            ),
            (
                RefreshRequestState::Published,
                "completed_with_rejections",
                "completed_with_diagnostics",
                false,
                "History refresh complete with issues",
            ),
            (
                RefreshRequestState::Failed,
                "source_refresh_failed",
                "internal",
                true,
                "History refresh failed",
            ),
        ];
        for (state, code, class, failure, label) in cases {
            let snapshot = terminal_status(state, code, class, failure);
            assert!(snapshot.is_terminal());
            assert_eq!(refresh_label(&snapshot), label);
        }

        let physically_committed = active_status(RefreshLogicalPhase::Direct, "committed", true, 0);
        assert!(!physically_committed.is_terminal());
    }
}
