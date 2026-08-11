use ctx_history_capture_model::{ProviderImportSummary, ProviderImportWorkResult};

use crate::SourceStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    Success,
    Failure,
    CompletedWithRejections,
    CompletedWithSourceFailures,
    CompletedWithRejectionsAndSourceFailures,
}

impl ImportOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::CompletedWithSourceFailures => "completed_with_source_failures",
            Self::CompletedWithRejectionsAndSourceFailures => {
                "completed_with_rejections_and_source_failures"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureScope {
    None,
    Record,
    Source,
    RecordAndSource,
}

impl ImportFailureScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Record => "record",
            Self::Source => "source",
            Self::RecordAndSource => "record_and_source",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailureType {
    None,
    RecordRejection,
    SourceFailure,
    RecordRejectionAndSourceFailure,
}

impl ImportFailureType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::SourceFailure => "source_failure",
            Self::RecordRejectionAndSourceFailure => "record_rejection_and_source_failure",
        }
    }
}

/// Typed outcome totals. Presentation layers decide which fields are rendered;
/// these facts do not imply a per-record or per-run Core delta.
#[derive(Debug, Clone, Default)]
pub struct ImportTotals {
    pub per_run_counts_available: bool,
    pub terminal_route_counts_available: bool,
    pub source_files: usize,
    pub source_bytes: u64,
    pub imported_sources: usize,
    pub sources_completed_with_rejections: usize,
    pub failed_sources: usize,
    pub imported_sessions: usize,
    pub imported_events: usize,
    pub imported_edges: usize,
    pub skipped_sessions: usize,
    pub skipped_events: usize,
    pub skipped_edges: usize,
    pub skipped: usize,
    pub failed: usize,
    pub current_source_count: Option<usize>,
    pub current_indexed_documents: Option<u64>,
    pub current_complete_records: Option<u64>,
    pub current_retained_records: Option<u64>,
    pub current_rejected_records: Option<u64>,
    pub current_ignored_records: Option<u64>,
    pub current_certified_source_bytes: Option<u64>,
    pub current_sources_with_rejections: Option<usize>,
    pub removed_source_count: Option<usize>,
    pub capture_work_remaining: bool,
    pub work_result: ProviderImportWorkResult,
}

impl ImportTotals {
    pub fn has_usable_source_result(&self) -> bool {
        self.imported_sources > 0 || self.current_source_count.unwrap_or_default() > 0
    }
    pub fn reported_source_failures(&self) -> usize {
        self.failed_sources
    }
    pub fn outcome(&self) -> (ImportOutcome, ImportFailureScope) {
        if !self.has_usable_source_result() && self.failed_sources > 0 {
            return (ImportOutcome::Failure, ImportFailureScope::Source);
        }
        match (self.failed_sources > 0, self.failed > 0) {
            (false, false) => (ImportOutcome::Success, ImportFailureScope::None),
            (false, true) => (
                ImportOutcome::CompletedWithRejections,
                ImportFailureScope::Record,
            ),
            (true, false) => (
                ImportOutcome::CompletedWithSourceFailures,
                ImportFailureScope::Source,
            ),
            (true, true) => (
                ImportOutcome::CompletedWithRejectionsAndSourceFailures,
                ImportFailureScope::RecordAndSource,
            ),
        }
    }
    pub fn failure_type(&self) -> ImportFailureType {
        match (self.failed_sources > 0, self.failed > 0) {
            (false, false) => ImportFailureType::None,
            (false, true) => ImportFailureType::RecordRejection,
            (true, false) => ImportFailureType::SourceFailure,
            (true, true) => ImportFailureType::RecordRejectionAndSourceFailure,
        }
    }
    pub fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.per_run_counts_available = true;
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.sources_completed_with_rejections += usize::from(summary.failed > 0);
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped_sessions += summary.skipped_sessions;
        self.skipped_events += summary.skipped_events;
        self.skipped_edges += summary.skipped_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
        self.capture_work_remaining |= summary.work_remaining;
        self.work_result = self.work_result.merge(summary.work_result());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_outcomes_preserve_failure_and_partial_truth() {
        for (failed_sources, rejected, outcome, scope, failure_type) in [
            (0, 0, "success", "none", "none"),
            (
                0,
                2,
                "completed_with_rejections",
                "record",
                "record_rejection",
            ),
            (
                2,
                0,
                "completed_with_source_failures",
                "source",
                "source_failure",
            ),
            (
                2,
                3,
                "completed_with_rejections_and_source_failures",
                "record_and_source",
                "record_rejection_and_source_failure",
            ),
        ] {
            let totals = ImportTotals {
                failed_sources,
                failed: rejected,
                current_source_count: Some(1),
                ..ImportTotals::default()
            };
            let (actual_outcome, actual_scope) = totals.outcome();
            assert_eq!(actual_outcome.as_str(), outcome);
            assert_eq!(actual_scope.as_str(), scope);
            assert_eq!(totals.failure_type().as_str(), failure_type);
        }
        let failed = ImportTotals {
            failed_sources: 1,
            ..ImportTotals::default()
        };
        assert_eq!(failed.outcome().0, ImportOutcome::Failure);
    }
}
