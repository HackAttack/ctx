use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::{format_bytes, format_count};

use super::{fields, progress, Field, Progress};
use crate::ui::{Document, RenderContext};

const MAX_DYNAMIC_TEXT_BYTES: usize = 256;

/// Terminal-neutral presentation view of a refresh status. Composition code
/// converts its domain snapshot to this owned value before rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshProgressSnapshot {
    schema_v1_fields: Value,
    request_id: Option<String>,
    kind: RefreshStatusKind,
    progress: SourceBackedRefreshProgress,
    total_sources_known: bool,
}

impl RefreshProgressSnapshot {
    pub fn from_schema_v1(fields: &Value) -> Result<Self> {
        let object = fields.as_object().ok_or_else(|| anyhow!("refresh status is not an object"))?;
        let request_state = parse_request_state(required_string(object, "request_state")?)?;
        let progress_fields = object.get("progress").and_then(Value::as_object)
            .ok_or_else(|| anyhow!("refresh status has no progress object"))?;
        let progress = SourceBackedRefreshProgress {
            phase: required_string(progress_fields, "phase")?.to_owned(),
            completed_sources: required_u64(progress_fields, "completed_sources")?,
            total_sources: required_u64(progress_fields, "total_sources")?,
            current_source: optional_string(progress_fields, "current_source"),
            completed_records: optional_u64(progress_fields, "completed_records"),
            completed_bytes: optional_u64(progress_fields, "completed_bytes"),
            current_source_progress: progress_fields.get("current_source_progress")
                .filter(|value| !value.is_null()).map(parse_current_source_progress).transpose()?,
        };
        let kind = if object.contains_key("logical_phase") {
            RefreshStatusKind::Logical(RefreshLogicalStatus {
                logical_phase: parse_logical_phase(required_string(object, "logical_phase")?)?,
                physical_attempt_id: required_string(object, "physical_attempt_id")?.to_owned(),
                physical_attempt_state: parse_request_state(required_string(object, "physical_attempt_state")?)?,
                progress_owner_request_id: required_string(object, "progress_owner_request_id")?.to_owned(),
                progress_owner_attempt_state: parse_request_state(required_string(object, "progress_owner_attempt_state")?)?,
                structured_outcome: object.get("structured_outcome").filter(|value| !value.is_null())
                    .map(parse_outcome).transpose()?,
            })
        } else if object.contains_key("maintenance_wake") {
            RefreshStatusKind::BackgroundMaintenanceWake(())
        } else {
            RefreshStatusKind::Legacy { request_state }
        };
        Ok(Self {
            schema_v1_fields: fields.clone(),
            request_id: optional_string(object, "request_id"),
            kind,
            total_sources_known: object.get("total_sources_known").and_then(Value::as_bool)
                .or_else(|| progress_fields.get("total_sources_known").and_then(Value::as_bool))
                .unwrap_or(false),
            progress,
        })
    }

    pub const fn kind(&self) -> &RefreshStatusKind {
        &self.kind
    }

    pub const fn progress(&self) -> &SourceBackedRefreshProgress {
        &self.progress
    }

    pub const fn total_sources_known(&self) -> bool {
        self.total_sources_known
    }

    pub fn schema_v1_fields(&self) -> &Value {
        &self.schema_v1_fields
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
            SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            | SourceBackedCurrentSourceProgressStage::OnlineBackup => current
                .snapshot_bytes_completed
                .zip(current.snapshot_bytes_total)
                .unwrap_or((0, 0)),
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            | SourceBackedCurrentSourceProgressStage::LogicalScan => (0, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRequestState { AdmissionPending, Queued, Running, Published, Failed }
impl RefreshRequestState { pub const fn is_terminal(self) -> bool { matches!(self, Self::Published | Self::Failed) } }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshLogicalPhase { Waiting, Attached, CoverageCheck, ExactSuccessor, Direct, Terminal }
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshStatusKind { Legacy { request_state: RefreshRequestState }, BackgroundMaintenanceWake(()), Logical(RefreshLogicalStatus) }
impl RefreshStatusKind { pub const fn request_state(&self) -> RefreshRequestState { match self { Self::Legacy { request_state } => *request_state, Self::BackgroundMaintenanceWake(()) => RefreshRequestState::Queued, Self::Logical(logical) => logical.physical_attempt_state } } }
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshLogicalStatus { pub logical_phase: RefreshLogicalPhase, pub physical_attempt_id: String, pub physical_attempt_state: RefreshRequestState, pub progress_owner_request_id: String, pub progress_owner_attempt_state: RefreshRequestState, pub structured_outcome: Option<RefreshStructuredOutcome> }
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStructuredOutcome { pub code: String }
impl RefreshStructuredOutcome { fn is_failure(&self) -> bool { self.code == "failed" || self.code.ends_with("_failed") } }
#[derive(Debug, Clone, PartialEq)]
pub struct SourceBackedRefreshProgress { pub phase: String, pub completed_sources: u64, pub total_sources: u64, pub current_source: Option<String>, pub completed_records: Option<u64>, pub completed_bytes: Option<u64>, pub current_source_progress: Option<SourceBackedCurrentSourceProgress> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedCurrentSourceProgressStage { SourceFamilyCopy, OnlineBackup, LogicalFingerprint, LogicalScan }
impl SourceBackedCurrentSourceProgressStage { pub const fn as_str(self) -> &'static str { match self { Self::SourceFamilyCopy => "source_family_copy", Self::OnlineBackup => "online_backup", Self::LogicalFingerprint => "logical_fingerprint", Self::LogicalScan => "logical_scan" } } }
#[derive(Debug, Clone, PartialEq)]
pub struct SourceBackedCurrentSourceProgress { pub stage: SourceBackedCurrentSourceProgressStage, pub snapshot_bytes_completed: Option<u64>, pub snapshot_bytes_total: Option<u64>, pub fields: Value }

fn required_string<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Result<&'a str> { object.get(name).and_then(Value::as_str).ok_or_else(|| anyhow!("refresh status {name} is not a string")) }
fn optional_string(object: &serde_json::Map<String, Value>, name: &str) -> Option<String> { object.get(name).and_then(Value::as_str).map(ToOwned::to_owned) }
fn required_u64(object: &serde_json::Map<String, Value>, name: &str) -> Result<u64> { object.get(name).and_then(Value::as_u64).ok_or_else(|| anyhow!("refresh progress {name} is not a u64")) }
fn optional_u64(object: &serde_json::Map<String, Value>, name: &str) -> Option<u64> { object.get(name).and_then(Value::as_u64) }
fn parse_request_state(value: &str) -> Result<RefreshRequestState> { Ok(match value { "admission_pending" => RefreshRequestState::AdmissionPending, "queued" => RefreshRequestState::Queued, "running" => RefreshRequestState::Running, "published" => RefreshRequestState::Published, "failed" => RefreshRequestState::Failed, _ => return Err(anyhow!("unknown refresh request state {value}")), }) }
fn parse_logical_phase(value: &str) -> Result<RefreshLogicalPhase> { Ok(match value { "waiting" => RefreshLogicalPhase::Waiting, "attached" => RefreshLogicalPhase::Attached, "coverage_check" => RefreshLogicalPhase::CoverageCheck, "exact_successor" => RefreshLogicalPhase::ExactSuccessor, "direct" => RefreshLogicalPhase::Direct, "terminal" => RefreshLogicalPhase::Terminal, _ => return Err(anyhow!("unknown refresh logical phase {value}")), }) }
fn parse_outcome(value: &Value) -> Result<RefreshStructuredOutcome> { Ok(RefreshStructuredOutcome { code: value.get("code").and_then(Value::as_str).ok_or_else(|| anyhow!("refresh outcome code is not a string"))?.to_owned() }) }
fn parse_current_source_progress(value: &Value) -> Result<SourceBackedCurrentSourceProgress> { let object = value.as_object().ok_or_else(|| anyhow!("current source progress is not an object"))?; let stage = match required_string(object, "stage")? { "source_family_copy" => SourceBackedCurrentSourceProgressStage::SourceFamilyCopy, "online_backup" => SourceBackedCurrentSourceProgressStage::OnlineBackup, "logical_fingerprint" => SourceBackedCurrentSourceProgressStage::LogicalFingerprint, "logical_scan" => SourceBackedCurrentSourceProgressStage::LogicalScan, other => return Err(anyhow!("unknown source progress stage {other}")), }; Ok(SourceBackedCurrentSourceProgress { stage, snapshot_bytes_completed: optional_u64(object, "snapshot_bytes_completed"), snapshot_bytes_total: optional_u64(object, "snapshot_bytes_total"), fields: value.clone() }) }

pub fn refresh_progress(
    context: &RenderContext,
    snapshot: &RefreshProgressSnapshot,
) -> Document {
    let completed = snapshot.progress.completed_sources as u64;
    let total = snapshot
        .total_sources_known
        .then_some(snapshot.progress.total_sources as u64)
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
        RefreshStatusKind::BackgroundMaintenanceWake(_) => "History refresh is queued",
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
        RefreshStatusKind::BackgroundMaintenanceWake(_) => "waiting",
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
    use serde_json::{json, Value};

    use super::*;
    use crate::ui::{StreamKind, TestContext};

    fn active_status(logical_phase: &str, physical_phase: &str, known: bool, total: u64) -> Value {
        json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": logical_phase,
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "running",
            "progress_owner_request_id": "progress-owner",
            "progress_owner_attempt_state": "running",
            "progress": {
                "phase": physical_phase,
                "completed_sources": 0,
                "total_sources": total,
                "total_sources_known": known,
                "current_source": Value::Null,
                "completed_records": Value::Null,
                "completed_bytes": Value::Null,
                "current_source_progress": Value::Null,
            }
        })
    }

    fn terminal_status(state: &str, code: &str, class: &str) -> Value {
        json!({
            "request_id": "logical-request",
            "request_state": state,
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": state,
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": state,
            "structured_outcome": {
                "code": code,
                "class": class,
                "retryable": false,
                "affected_routes": [],
                "retryable_routes": [],
                "blocked_routes": [],
                "physical_attempt_id": "physical-attempt",
            },
            "progress": {
                "phase": "committed",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": true,
            }
        })
    }

    #[test]
    fn full_status_adapter_preserves_logical_phases_and_physical_owner() {
        for (phase, expected) in [
            ("waiting", "History refresh is waiting"),
            ("attached", "Refreshing history with shared work"),
            ("coverage_check", "Checking refresh coverage"),
            ("exact_successor", "Waiting for successor refresh"),
        ] {
            let snapshot = RefreshProgressSnapshot::from_schema_v1(&active_status(
                phase,
                "committed",
                true,
                2,
            ))
            .unwrap();
            assert_eq!(refresh_label(&snapshot), expected);
            assert!(!snapshot.is_terminal(), "logical phase {phase}");
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
        let known = RefreshProgressSnapshot::from_schema_v1(&active_status(
            "direct",
            "discovering",
            true,
            0,
        ))
        .unwrap();
        let unknown = RefreshProgressSnapshot::from_schema_v1(&active_status(
            "direct",
            "discovering",
            false,
            0,
        ))
        .unwrap();
        assert!(refresh_progress(&context, &known)
            .render_plain()
            .contains("0 / 0"));
        assert!(refresh_progress(&context, &unknown)
            .render_plain()
            .contains("measuring"));
    }

    #[test]
    fn byte_progress_requires_one_complete_engine_snapshot_pair() {
        let mut status = active_status("attached", "copying", true, 2);
        status["progress"]["current_source_progress"] = json!({
            "stage": "source_family_copy",
            "snapshot_bytes_completed": 256,
            "snapshot_bytes_total": 512,
        });
        let paired = RefreshProgressSnapshot::from_schema_v1(&status).unwrap();
        assert_eq!(paired.byte_progress(), (256, 512));

        status["progress"]["current_source_progress"]
            .as_object_mut()
            .unwrap()
            .remove("snapshot_bytes_total");
        let partial = RefreshProgressSnapshot::from_schema_v1(&status).unwrap();
        assert_eq!(partial.byte_progress(), (0, 0));
    }

    #[test]
    fn structured_terminal_outcome_alone_decides_done() {
        let cases = [
            (
                "published",
                "completed",
                "completed",
                "History refresh complete",
            ),
            (
                "published",
                "completed_with_rejections",
                "completed_with_diagnostics",
                "History refresh complete with issues",
            ),
            (
                "failed",
                "source_refresh_failed",
                "internal",
                "History refresh failed",
            ),
        ];
        for (state, code, class, label) in cases {
            let snapshot =
                RefreshProgressSnapshot::from_schema_v1(&terminal_status(state, code, class))
                    .unwrap();
            assert!(snapshot.is_terminal());
            assert_eq!(refresh_label(&snapshot), label);
        }

        let physically_committed =
            RefreshProgressSnapshot::from_schema_v1(&active_status("direct", "committed", true, 0))
                .unwrap();
        assert!(!physically_committed.is_terminal());
    }
}
