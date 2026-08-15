#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CandidateCloneMetrics {
    pub retained_reflinked_files: usize,
    pub retained_hardlinked_files: usize,
    pub retained_copied_files: usize,
    pub retained_copied_bytes: u64,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateCloneMetrics {
    pub(super) retained_reflinked_files: usize,
    pub(super) retained_hardlinked_files: usize,
    pub(super) retained_copied_files: usize,
    pub(super) retained_copied_bytes: u64,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CANDIDATE_CLONE_METRICS: std::cell::Cell<CandidateCloneMetrics> = const {
        std::cell::Cell::new(CandidateCloneMetrics {
            retained_reflinked_files: 0,
            retained_hardlinked_files: 0,
            retained_copied_files: 0,
            retained_copied_bytes: 0,
        })
    };
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_candidate_clone_metrics(metrics: CandidateCloneMetrics) {
    CANDIDATE_CLONE_METRICS.with(|slot| slot.set(metrics));
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_candidate_clone_metrics(_metrics: CandidateCloneMetrics) {}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_candidate_clone_metrics() {
    CANDIDATE_CLONE_METRICS.with(|slot| slot.set(CandidateCloneMetrics::default()));
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_clone_metrics() -> CandidateCloneMetrics {
    CANDIDATE_CLONE_METRICS.with(std::cell::Cell::get)
}
