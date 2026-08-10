use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionPending => "admission_pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    pub(super) fn is_active(self) -> bool {
        matches!(self, Self::AdmissionPending | Self::Queued | Self::Running)
    }
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshProgress {
    pub phase: String,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            current_source_progress: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    pub(super) fn to_json_with_total_known(&self, total_sources_known: bool) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "total_sources_known": total_sources_known,
            "current_source": self.current_source,
            "completed_records": self.completed_records,
            "completed_bytes": self.completed_bytes,
            "current_source_progress": self.current_source_progress
                .map(SourceBackedCurrentSourceProgress::to_json),
        }))
    }

    pub fn from_status_json(response: &Value) -> Result<Self> {
        let progress = response
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        let phase = progress
            .get("phase")
            .and_then(Value::as_str)
            .filter(|phase| !phase.is_empty())
            .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid phase"))?
            .to_owned();
        let current_source = match progress.get("current_source") {
            None | Some(Value::Null) => None,
            Some(Value::String(source)) => Some(source.clone()),
            Some(_) => bail!("daemon source refresh progress has an invalid current_source"),
        };
        let current_source_progress = match progress.get("current_source_progress") {
            None | Some(Value::Null) => None,
            Some(value) => Some(SourceBackedCurrentSourceProgress::from_json(value)?),
        };
        Ok(Self {
            phase,
            completed_sources: required_progress_usize(progress, "completed_sources")?,
            total_sources: required_progress_usize(progress, "total_sources")?,
            current_source,
            completed_records: optional_progress_u64(progress, "completed_records")?,
            completed_bytes: optional_progress_u64(progress, "completed_bytes")?,
            current_source_progress,
        })
    }
}

pub(super) fn status_progress_total_sources_known(response: &Value) -> bool {
    let Some(progress) = response.get("progress") else {
        return false;
    };
    match progress.get("total_sources_known") {
        Some(Value::Bool(known)) => *known,
        // Pre-additive durable records used zero as the unknown placeholder.
        // A new known-zero snapshot carries the explicit boolean above.
        None => progress
            .get("total_sources")
            .and_then(Value::as_u64)
            .is_some_and(|total| total != 0),
        Some(_) => false,
    }
}

fn required_progress_usize(fields: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    fields
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}"))
}

fn optional_progress_u64(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress has an invalid {field}")
        }),
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn progress_parser_distinguishes_legacy_placeholder_and_additive_known_zero() {
        let legacy_unknown = json!({
            "progress": {
                "phase": "queued",
                "completed_sources": 0,
                "total_sources": 0,
            }
        });
        let legacy_known = json!({
            "progress": {
                "phase": "refreshing",
                "completed_sources": 1,
                "total_sources": 2,
            }
        });
        let additive_known_zero = json!({
            "progress": {
                "phase": "published",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": true,
            }
        });

        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&legacy_unknown)
                .unwrap()
                .total_sources,
            0
        );
        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&additive_known_zero)
                .unwrap()
                .total_sources,
            0
        );
        assert!(!status_progress_total_sources_known(&legacy_unknown));
        assert!(status_progress_total_sources_known(&legacy_known));
        assert!(status_progress_total_sources_known(&additive_known_zero));
    }
}
