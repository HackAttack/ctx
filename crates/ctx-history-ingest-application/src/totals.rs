use ctx_history_capture_model::{ProviderImportSummary, ProviderImportWorkResult};

use crate::SourceStats;

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
