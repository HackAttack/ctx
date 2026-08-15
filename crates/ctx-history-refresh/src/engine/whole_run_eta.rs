use super::*;

/// Cold setup tails track the runtime profile that produced the scan much
/// better than source bytes alone. Retain a small fixed floor for short runs.
const MIN_POST_SCAN_TAIL_MILLIS: u64 = 3_000;
const POST_SCAN_TAIL_DIVISOR: u64 = 25;
const MIN_WARMUP_MILLIS: u64 = 8_000;
const MIN_WARMUP_BYTES: u64 = 32 * 1024 * 1024;
const MIN_ADVANCING_SAMPLES: u8 = 4;
const QUALIFICATION_WINDOW_MILLIS: u64 = 35_000;
const SHORT_RUN_FALLBACK_PERCENT: u64 = 50;
const MIN_DEADLINE_CHANGE_MILLIS: u64 = 2_000;
const STALL_TIMEOUT_MILLIS: u64 = 5_000;
const MIN_USEFUL_REMAINING_MILLIS: u64 = 2_000;

#[derive(Debug, Clone)]
pub(super) struct WholeRunEtaEstimator {
    eligible: bool,
    disabled: bool,
    expired: bool,
    total_bytes: Option<u64>,
    last_completed_bytes: u64,
    last_elapsed_millis: u64,
    last_advance_elapsed_millis: Option<u64>,
    rate_anchor_completed_bytes: Option<u64>,
    rate_anchor_elapsed_millis: Option<u64>,
    requalifying_after_stall: bool,
    advancing_samples: u8,
    sustained_later_samples: u8,
    qualification_candidate_deadline_millis: Option<u64>,
    qualification_started_elapsed_millis: Option<u64>,
    accepted_deadline_millis: Option<u64>,
}

impl WholeRunEtaEstimator {
    pub(super) fn new(eligible: bool) -> Self {
        Self {
            eligible,
            disabled: false,
            expired: false,
            total_bytes: None,
            last_completed_bytes: 0,
            last_elapsed_millis: 0,
            last_advance_elapsed_millis: None,
            rate_anchor_completed_bytes: None,
            rate_anchor_elapsed_millis: None,
            requalifying_after_stall: false,
            advancing_samples: 0,
            sustained_later_samples: 0,
            qualification_candidate_deadline_millis: None,
            qualification_started_elapsed_millis: None,
            accepted_deadline_millis: None,
        }
    }

    pub(super) fn disable(&mut self) {
        self.disabled = true;
        self.accepted_deadline_millis = None;
    }

    pub(super) fn clear(&mut self) {
        self.accepted_deadline_millis = None;
        self.expired = true;
    }

    pub(super) fn update(
        &mut self,
        stage: SourceBackedRefreshStage,
        exact: Option<SourceBackedExactScanProgress>,
        elapsed_millis: Option<u64>,
    ) {
        if !self.eligible || self.disabled || self.expired {
            return;
        }
        if matches!(
            stage,
            SourceBackedRefreshStage::Complete | SourceBackedRefreshStage::Failed
        ) {
            self.clear();
            return;
        }
        let Some(exact) = exact else {
            if self.total_bytes.is_some() {
                self.disable();
            }
            return;
        };
        let Some(elapsed_millis) = elapsed_millis else {
            self.disable();
            return;
        };
        if exact.total_bytes == 0
            || exact.completed_bytes > exact.total_bytes
            || self
                .total_bytes
                .is_some_and(|total| total != exact.total_bytes)
            || exact.completed_bytes < self.last_completed_bytes
            || elapsed_millis < self.last_elapsed_millis
        {
            self.disable();
            return;
        }
        self.total_bytes = Some(exact.total_bytes);

        if stage != SourceBackedRefreshStage::Reading {
            if exact.completed_bytes != exact.total_bytes {
                self.disable();
                return;
            }
            if self
                .accepted_deadline_millis
                .is_some_and(|deadline| deadline <= elapsed_millis)
            {
                self.clear();
                return;
            }
            self.last_completed_bytes = exact.completed_bytes;
            self.last_elapsed_millis = elapsed_millis;
            return;
        }

        let advanced = exact.completed_bytes > self.last_completed_bytes;
        if self.rate_anchor_completed_bytes.is_none() {
            self.requalifying_after_stall = false;
            self.reset_rate_window(exact.completed_bytes, elapsed_millis);
            self.last_completed_bytes = exact.completed_bytes;
            self.last_elapsed_millis = elapsed_millis;
            return;
        }
        if !advanced {
            self.last_elapsed_millis = elapsed_millis;
            if self
                .accepted_deadline_millis
                .is_some_and(|deadline| deadline <= elapsed_millis)
            {
                self.clear();
                return;
            }
            if exact.completed_bytes != exact.total_bytes
                && self
                    .last_advance_elapsed_millis
                    .is_some_and(|last_advance| {
                        elapsed_millis.saturating_sub(last_advance) >= STALL_TIMEOUT_MILLIS
                    })
            {
                self.suppress_for_stall();
            }
            return;
        }
        if self.requalifying_after_stall {
            self.requalifying_after_stall = false;
            self.reset_rate_window(exact.completed_bytes, elapsed_millis);
            self.last_completed_bytes = exact.completed_bytes;
            self.last_elapsed_millis = elapsed_millis;
            return;
        }
        if self
            .accepted_deadline_millis
            .is_some_and(|deadline| deadline <= elapsed_millis)
        {
            self.clear();
            return;
        }
        self.advancing_samples = self.advancing_samples.saturating_add(1);
        self.last_advance_elapsed_millis = Some(elapsed_millis);
        self.last_completed_bytes = exact.completed_bytes;
        self.last_elapsed_millis = elapsed_millis;
        if exact.completed_bytes == exact.total_bytes {
            if self.accepted_deadline_millis.is_some() {
                self.accepted_deadline_millis =
                    Some(elapsed_millis.saturating_add(post_scan_tail_millis(elapsed_millis)));
            }
            return;
        }
        if exact.completed_bytes == 0 {
            return;
        }

        let Some((candidate, window_millis, window_bytes)) = candidate_deadline_millis(
            exact,
            elapsed_millis,
            self.rate_anchor_completed_bytes
                .unwrap_or(exact.completed_bytes),
            self.rate_anchor_elapsed_millis.unwrap_or(elapsed_millis),
        ) else {
            return;
        };
        if self.accepted_deadline_millis.is_none() {
            let qualified_by_stability =
                self.observe_qualification_candidate(candidate, elapsed_millis);
            if window_millis >= MIN_WARMUP_MILLIS
                && window_bytes >= MIN_WARMUP_BYTES.min(exact.total_bytes / 4)
                && self.advancing_samples >= MIN_ADVANCING_SAMPLES
                && (qualified_by_stability
                    || has_short_run_fallback_progress(exact.completed_bytes, exact.total_bytes))
            {
                self.accepted_deadline_millis = Some(candidate.max(elapsed_millis + 1));
                self.clear_qualification_candidate();
            }
            return;
        }

        let accepted = self.accepted_deadline_millis.unwrap_or(candidate);
        if candidate < accepted {
            // Optimistic changes move only one quarter of the way per sample.
            self.accepted_deadline_millis = Some(accepted - (accepted - candidate) / 4);
            self.sustained_later_samples = 0;
        } else if candidate > accepted.saturating_add(material_change(accepted)) {
            self.sustained_later_samples = self.sustained_later_samples.saturating_add(1);
            if self.sustained_later_samples >= 2 {
                self.accepted_deadline_millis = Some(candidate);
                self.sustained_later_samples = 0;
            }
        } else {
            // Small pessimistic revisions are accepted immediately; only
            // materially later deadlines require sustained evidence.
            self.accepted_deadline_millis = Some(candidate);
            self.sustained_later_samples = 0;
        }
    }

    fn observe_qualification_candidate(
        &mut self,
        candidate_deadline_millis: u64,
        elapsed_millis: u64,
    ) -> bool {
        match (
            self.qualification_candidate_deadline_millis,
            self.qualification_started_elapsed_millis,
        ) {
            (Some(anchor), Some(started))
                if candidate_deadline_millis.abs_diff(anchor) <= qualification_change(anchor) =>
            {
                elapsed_millis.saturating_sub(started) >= QUALIFICATION_WINDOW_MILLIS
            }
            _ => {
                self.qualification_candidate_deadline_millis = Some(candidate_deadline_millis);
                self.qualification_started_elapsed_millis = Some(elapsed_millis);
                false
            }
        }
    }

    fn clear_qualification_candidate(&mut self) {
        self.qualification_candidate_deadline_millis = None;
        self.qualification_started_elapsed_millis = None;
    }

    fn reset_rate_window(&mut self, completed_bytes: u64, elapsed_millis: u64) {
        self.rate_anchor_completed_bytes = Some(completed_bytes);
        self.rate_anchor_elapsed_millis = Some(elapsed_millis);
        self.last_advance_elapsed_millis = Some(elapsed_millis);
        self.advancing_samples = 0;
        self.sustained_later_samples = 0;
        self.clear_qualification_candidate();
        self.accepted_deadline_millis = None;
    }

    fn suppress_for_stall(&mut self) {
        self.accepted_deadline_millis = None;
        self.rate_anchor_completed_bytes = None;
        self.rate_anchor_elapsed_millis = None;
        self.clear_qualification_candidate();
        self.advancing_samples = 0;
        self.sustained_later_samples = 0;
        self.requalifying_after_stall = true;
    }

    pub(super) fn estimated_remaining_millis(&self) -> Option<u64> {
        if !self.eligible || self.disabled || self.expired {
            return None;
        }
        self.accepted_deadline_millis
            .and_then(|deadline| deadline.checked_sub(self.last_elapsed_millis))
            .filter(|remaining| *remaining > MIN_USEFUL_REMAINING_MILLIS)
    }
}

fn material_change(deadline_millis: u64) -> u64 {
    (deadline_millis / 10).max(MIN_DEADLINE_CHANGE_MILLIS)
}

fn qualification_change(deadline_millis: u64) -> u64 {
    (deadline_millis / 20).max(MIN_DEADLINE_CHANGE_MILLIS)
}

fn has_short_run_fallback_progress(completed_bytes: u64, total_bytes: u64) -> bool {
    u128::from(completed_bytes) * 100
        >= u128::from(total_bytes) * u128::from(SHORT_RUN_FALLBACK_PERCENT)
}

fn candidate_deadline_millis(
    exact: SourceBackedExactScanProgress,
    elapsed_millis: u64,
    anchor_completed_bytes: u64,
    anchor_elapsed_millis: u64,
) -> Option<(u64, u64, u64)> {
    let window_millis = elapsed_millis.checked_sub(anchor_elapsed_millis)?;
    let window_bytes = exact.completed_bytes.checked_sub(anchor_completed_bytes)?;
    if window_millis == 0 || window_bytes == 0 {
        return None;
    }
    let remaining_bytes = exact.total_bytes.checked_sub(exact.completed_bytes)?;
    let scan_remaining = (u128::from(window_millis) * u128::from(remaining_bytes)
        / u128::from(window_bytes))
    .min(u128::from(u64::MAX)) as u64;
    let uncertainty_reserve = uncertainty_reserve_millis(window_millis, scan_remaining);
    let projected_scan_duration = elapsed_millis.saturating_add(scan_remaining);
    Some((
        projected_scan_duration
            .saturating_add(uncertainty_reserve)
            .saturating_add(post_scan_tail_millis(projected_scan_duration)),
        window_millis,
        window_bytes,
    ))
}

fn uncertainty_reserve_millis(window_millis: u64, scan_remaining_millis: u64) -> u64 {
    let tapered_scan_reserve =
        (u128::from(scan_remaining_millis) * 3 / 5).min(u128::from(u64::MAX)) as u64;
    (window_millis / 3).min(tapered_scan_reserve)
}

fn post_scan_tail_millis(projected_scan_duration_millis: u64) -> u64 {
    (projected_scan_duration_millis / POST_SCAN_TAIL_DIVISOR).max(MIN_POST_SCAN_TAIL_MILLIS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(total_mib: u64, completed_mib: u64) -> SourceBackedExactScanProgress {
        SourceBackedExactScanProgress {
            total_bytes: total_mib * 1024 * 1024,
            completed_bytes: completed_mib * 1024 * 1024,
        }
    }

    fn stable_candidate_exact(elapsed_millis: u64) -> SourceBackedExactScanProgress {
        let total_bytes = 10_000 * 1024 * 1024;
        let target_scan_deadline_millis = 100_000;
        let completed_bytes = (u128::from(elapsed_millis) * u128::from(total_bytes)
            / u128::from(target_scan_deadline_millis - elapsed_millis / 3))
        .min(u128::from(u64::MAX)) as u64;
        SourceBackedExactScanProgress {
            total_bytes,
            completed_bytes,
        }
    }

    fn warm(estimator: &mut WholeRunEtaEstimator) {
        for (elapsed, completed) in [
            (2_000, 40),
            (4_000, 80),
            (6_000, 120),
            (8_000, 160),
            (10_000, 200),
            (12_000, 240),
        ] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(400, completed)),
                Some(elapsed),
            );
        }
    }

    #[test]
    fn uncertainty_reserve_is_bounded_and_tapers_with_scan_remaining() {
        assert_eq!(uncertainty_reserve_millis(12_000, 100_000), 4_000);
        assert_eq!(uncertainty_reserve_millis(12_000, 2_000), 1_200);
        assert_eq!(uncertainty_reserve_millis(12_000, 1), 0);
        assert_eq!(uncertainty_reserve_millis(12_000, 0), 0);
        assert_eq!(uncertainty_reserve_millis(u64::MAX, u64::MAX), u64::MAX / 3);
    }

    #[test]
    fn compact_replay_scale_vectors_apply_reserve_and_runtime_scaled_tail() {
        for (total_bytes, window_millis, completed_bytes, expected_tail_millis) in [
            (431_014_000, 10_000, 215_507_000, 3_000),
            (4_098_579_000, 100_000, 2_049_289_500, 8_000),
            (34_167_005_000, 300_000, 17_083_502_500, 24_000),
        ] {
            let exact = SourceBackedExactScanProgress {
                total_bytes,
                completed_bytes,
            };
            let (candidate, _, _) = candidate_deadline_millis(exact, window_millis, 0, 0).unwrap();
            let raw_scan_remaining = window_millis;
            let reserve = window_millis / 3;
            assert_eq!(
                candidate,
                window_millis + raw_scan_remaining + reserve + expected_tail_millis
            );
        }
    }

    #[test]
    fn progress_and_deadline_math_are_integer_safe_at_u64_bounds() {
        let minimum_completed = u64::MAX / 2 + 1;
        assert!(has_short_run_fallback_progress(minimum_completed, u64::MAX));
        assert!(!has_short_run_fallback_progress(
            minimum_completed - 1,
            u64::MAX
        ));

        let exact = SourceBackedExactScanProgress {
            total_bytes: u64::MAX,
            completed_bytes: 1,
        };
        let (candidate, window_millis, window_bytes) =
            candidate_deadline_millis(exact, u64::MAX, 0, 0).unwrap();
        assert_eq!(candidate, u64::MAX);
        assert_eq!(window_millis, u64::MAX);
        assert_eq!(window_bytes, 1);
        assert_eq!(candidate_deadline_millis(exact, 0, 0, 1), None);
    }

    #[test]
    fn remaining_eta_is_visible_only_above_the_usefulness_floor() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        estimator.last_elapsed_millis = 10_000;

        estimator.accepted_deadline_millis = Some(12_001);
        assert_eq!(estimator.estimated_remaining_millis(), Some(2_001));

        estimator.accepted_deadline_millis = Some(12_000);
        assert_eq!(estimator.estimated_remaining_millis(), None);
        assert_eq!(estimator.accepted_deadline_millis, Some(12_000));

        estimator.accepted_deadline_millis = Some(10_050);
        assert_eq!(estimator.estimated_remaining_millis(), None);
        assert_eq!(estimator.accepted_deadline_millis, Some(10_050));
    }

    #[test]
    fn deterministic_vectors_cover_warmup_and_startup_bursts() {
        let mut startup_burst = WholeRunEtaEstimator::new(true);
        for (elapsed, completed) in [(500, 80), (1_000, 100), (2_000, 110), (4_000, 120)] {
            startup_burst.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(400, completed)),
                Some(elapsed),
            );
            assert_eq!(startup_burst.estimated_remaining_millis(), None);
        }

        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        assert!(estimator.estimated_remaining_millis().is_some());
        let candidate_without_tail = 12_000 * (400 - 240) / 240;
        assert!(estimator.estimated_remaining_millis().unwrap() > candidate_without_tail);
    }

    #[test]
    fn stable_candidate_qualifies_after_thirty_five_seconds() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        for elapsed in (0..=40_000).step_by(5_000) {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(stable_candidate_exact(elapsed)),
                Some(elapsed),
            );
            if elapsed < 40_000 {
                assert_eq!(estimator.estimated_remaining_millis(), None);
            }
        }

        let exact = stable_candidate_exact(40_000);
        assert!(!has_short_run_fallback_progress(
            exact.completed_bytes,
            exact.total_bytes
        ));
        assert!(estimator.estimated_remaining_millis().is_some());
    }

    #[test]
    fn qualification_window_resets_after_out_of_band_candidate() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        assert!(!estimator.observe_qualification_candidate(100_000, 10_000));
        assert!(!estimator.observe_qualification_candidate(105_001, 20_000));
        assert_eq!(
            estimator.qualification_candidate_deadline_millis,
            Some(105_001)
        );
        assert_eq!(estimator.qualification_started_elapsed_millis, Some(20_000));
        assert!(!estimator.observe_qualification_candidate(105_001, 54_999));
        assert!(estimator.observe_qualification_candidate(105_001, 55_000));
    }

    #[test]
    fn qualification_band_boundary_is_inclusive() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        assert!(!estimator.observe_qualification_candidate(100_000, 10_000));
        assert_eq!(qualification_change(100_000), 5_000);
        assert!(estimator.observe_qualification_candidate(105_000, 45_000));
        assert_eq!(
            estimator.qualification_candidate_deadline_millis,
            Some(100_000)
        );
        assert_eq!(estimator.qualification_started_elapsed_millis, Some(10_000));
    }

    #[test]
    fn initial_eta_qualifies_at_fifty_percent_fallback() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(1_000, 0)),
            Some(0),
        );
        for (elapsed, completed) in [
            (2_000, 100),
            (4_000, 200),
            (6_000, 300),
            (8_000, 400),
            (9_999, 499),
        ] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(1_000, completed)),
                Some(elapsed),
            );
            assert_eq!(estimator.estimated_remaining_millis(), None);
        }

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(1_000, 500)),
            Some(10_000),
        );
        assert!(estimator.estimated_remaining_millis().is_some());
    }

    #[test]
    fn initial_eta_does_not_qualify_below_fifty_percent_fallback() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        for (elapsed, completed) in [
            (0, 0),
            (2_000, 100),
            (4_000, 200),
            (6_000, 300),
            (8_000, 400),
            (10_000, 499),
        ] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(1_000, completed)),
                Some(elapsed),
            );
        }

        assert!(estimator.advancing_samples >= MIN_ADVANCING_SAMPLES);
        assert_eq!(estimator.estimated_remaining_millis(), None);
    }

    #[test]
    fn slowdown_moves_deadline_later_only_after_sustained_samples() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        let stable = estimator.accepted_deadline_millis.unwrap();
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 250)),
            Some(15_000),
        );
        assert!(
            estimator.accepted_deadline_millis.unwrap()
                <= stable.saturating_add(material_change(stable))
        );
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 260)),
            Some(18_000),
        );
        assert!(estimator.accepted_deadline_millis.unwrap() > stable);
    }

    #[test]
    fn discovery_delay_is_not_extrapolated_as_scan_time() {
        let mut baseline = WholeRunEtaEstimator::new(true);
        warm(&mut baseline);
        let baseline_deadline = baseline.accepted_deadline_millis.unwrap();

        let discovery_delay = 30_000;
        let mut delayed = WholeRunEtaEstimator::new(true);
        for (elapsed, completed) in [
            (2_000, 40),
            (4_000, 80),
            (6_000, 120),
            (8_000, 160),
            (10_000, 200),
            (12_000, 240),
        ] {
            delayed.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(400, completed)),
                Some(elapsed + discovery_delay),
            );
        }

        assert_eq!(
            delayed.accepted_deadline_millis.unwrap(),
            baseline_deadline + discovery_delay
        );
    }

    #[test]
    fn speedup_damps_only_the_optimistic_deadline_change() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        let accepted = estimator.accepted_deadline_millis.unwrap();
        let faster = exact(400, 300);
        let raw_candidate =
            candidate_deadline_millis(faster, 13_000, exact(400, 40).completed_bytes, 2_000)
                .unwrap()
                .0;

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(faster),
            Some(13_000),
        );

        let damped = estimator.accepted_deadline_millis.unwrap();
        assert!(damped < accepted);
        assert!(damped > raw_candidate);
    }

    #[test]
    fn stall_resets_in_progress_qualification_window() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        for elapsed in [0, 5_000, 10_000] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(stable_candidate_exact(elapsed)),
                Some(elapsed),
            );
        }
        assert!(estimator.qualification_candidate_deadline_millis.is_some());

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(stable_candidate_exact(10_000)),
            Some(15_000),
        );
        assert_eq!(estimator.qualification_candidate_deadline_millis, None);
        assert_eq!(estimator.qualification_started_elapsed_millis, None);
        assert!(estimator.requalifying_after_stall);

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(stable_candidate_exact(16_000)),
            Some(16_000),
        );
        assert_eq!(estimator.qualification_candidate_deadline_millis, None);
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(stable_candidate_exact(21_000)),
            Some(21_000),
        );
        assert_eq!(estimator.qualification_started_elapsed_millis, Some(21_000));
        assert_eq!(estimator.estimated_remaining_millis(), None);
    }

    #[test]
    fn stalled_scan_suppresses_eta_and_resume_must_requalify() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        assert!(estimator.estimated_remaining_millis().is_some());

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 240)),
            Some(16_999),
        );
        assert!(estimator.estimated_remaining_millis().is_some());
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 240)),
            Some(17_000),
        );
        assert_eq!(estimator.estimated_remaining_millis(), None);

        for (elapsed, completed) in [(18_000, 250), (20_000, 270), (22_000, 290), (24_000, 310)] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(400, completed)),
                Some(elapsed),
            );
            assert_eq!(estimator.estimated_remaining_millis(), None);
        }
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 330)),
            Some(26_000),
        );
        assert!(estimator.estimated_remaining_millis().is_some());
    }

    #[test]
    fn stage_handoff_counts_down_and_expiry_abstains() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        estimator.update(
            SourceBackedRefreshStage::Merging,
            Some(exact(400, 400)),
            Some(20_000),
        );
        let remaining = estimator.estimated_remaining_millis().unwrap();
        estimator.update(
            SourceBackedRefreshStage::Syncing,
            Some(exact(400, 400)),
            Some(20_500),
        );
        assert!(estimator.estimated_remaining_millis().unwrap() < remaining);
        estimator.update(
            SourceBackedRefreshStage::Activation,
            Some(exact(400, 400)),
            Some(u64::MAX),
        );
        assert_eq!(estimator.estimated_remaining_millis(), None);
    }

    #[test]
    fn scan_completion_reanchors_qualified_deadline_to_post_scan_tail() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        let scan_deadline = estimator.accepted_deadline_millis.unwrap();
        let completion_elapsed = 20_000;
        let expected_deadline = completion_elapsed + post_scan_tail_millis(completion_elapsed);
        assert_ne!(scan_deadline, expected_deadline);

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(completion_elapsed),
        );

        assert_eq!(estimator.accepted_deadline_millis, Some(expected_deadline));
        assert_eq!(
            estimator.estimated_remaining_millis(),
            Some(post_scan_tail_millis(completion_elapsed))
        );
    }

    #[test]
    fn unqualified_scan_completion_does_not_create_deadline() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        for (elapsed, completed) in [(0, 0), (2_000, 40), (4_000, 80)] {
            estimator.update(
                SourceBackedRefreshStage::Reading,
                Some(exact(400, completed)),
                Some(elapsed),
            );
        }
        assert_eq!(estimator.accepted_deadline_millis, None);

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(6_000),
        );
        assert_eq!(estimator.accepted_deadline_millis, None);
        assert_eq!(estimator.estimated_remaining_millis(), None);

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(7_000),
        );
        assert_eq!(estimator.accepted_deadline_millis, None);
        assert_eq!(estimator.estimated_remaining_millis(), None);
    }

    #[test]
    fn repeated_complete_reading_events_count_down_without_extending_deadline() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(20_000),
        );
        let deadline = estimator.accepted_deadline_millis.unwrap();
        let initial_remaining = estimator.estimated_remaining_millis().unwrap();

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(20_500),
        );
        assert_eq!(estimator.accepted_deadline_millis, Some(deadline));
        assert_eq!(
            estimator.estimated_remaining_millis(),
            Some(deadline - 20_500)
        );
        assert!(estimator.estimated_remaining_millis().unwrap() < initial_remaining);

        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 400)),
            Some(deadline),
        );
        assert_eq!(estimator.accepted_deadline_millis, None);
        assert!(estimator.expired);
        assert_eq!(estimator.estimated_remaining_millis(), None);
    }

    #[test]
    fn accounting_regression_abstention_reset_and_terminal_clear_are_fail_closed() {
        let mut estimator = WholeRunEtaEstimator::new(true);
        warm(&mut estimator);
        estimator.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(400, 10)),
            Some(13_000),
        );
        assert_eq!(estimator.estimated_remaining_millis(), None);

        let mut unsupported = WholeRunEtaEstimator::new(false);
        warm(&mut unsupported);
        assert_eq!(unsupported.estimated_remaining_millis(), None);

        let mut reset = WholeRunEtaEstimator::new(true);
        warm(&mut reset);
        reset.clear();
        assert_eq!(reset.estimated_remaining_millis(), None);

        let mut completed = WholeRunEtaEstimator::new(true);
        warm(&mut completed);
        completed.update(
            SourceBackedRefreshStage::Complete,
            Some(exact(400, 400)),
            Some(20_000),
        );
        assert_eq!(completed.estimated_remaining_millis(), None);

        let mut failed = WholeRunEtaEstimator::new(true);
        warm(&mut failed);
        failed.update(
            SourceBackedRefreshStage::Failed,
            Some(exact(400, 240)),
            Some(13_000),
        );
        assert_eq!(failed.estimated_remaining_millis(), None);
    }

    #[test]
    fn route_accounting_loss_and_disagreement_abstain() {
        let mut missing = WholeRunEtaEstimator::new(true);
        warm(&mut missing);
        missing.update(SourceBackedRefreshStage::Reading, None, Some(13_000));
        assert_eq!(missing.estimated_remaining_millis(), None);

        let mut changed = WholeRunEtaEstimator::new(true);
        warm(&mut changed);
        changed.update(
            SourceBackedRefreshStage::Reading,
            Some(exact(401, 250)),
            Some(13_000),
        );
        assert_eq!(changed.estimated_remaining_millis(), None);
    }
}
