use ctx_history_capture_model::{SourceBackedExactScanProgress, SourceBackedRecordProgressDelta};

#[derive(Debug, Default)]
pub(super) struct AttemptExactScanAccounting {
    total_bytes: u64,
    declared_routes: usize,
    completed_unaccounted_routes: usize,
    current_route_total: Option<u64>,
    current_route_completed: u64,
    current_route_observed_bytes: u64,
    completed_bytes: u64,
    invalid: bool,
}

impl AttemptExactScanAccounting {
    pub(super) fn revoke(&mut self) {
        self.invalid = true;
    }

    pub(super) fn begin_route(&mut self) {
        self.current_route_total = None;
        self.current_route_completed = 0;
        self.current_route_observed_bytes = 0;
    }

    pub(super) fn observe(&mut self, delta: &SourceBackedRecordProgressDelta) {
        let Some(observed_bytes) = self
            .current_route_observed_bytes
            .checked_add(delta.completed_bytes)
        else {
            self.invalid = true;
            return;
        };
        self.current_route_observed_bytes = observed_bytes;
        if let Some(total) = delta.exact_total_bytes {
            if self.current_route_total.is_some() {
                self.invalid = true;
                return;
            }
            let Some(attempt_total) = self.total_bytes.checked_add(total) else {
                self.invalid = true;
                return;
            };
            self.current_route_total = Some(total);
            self.total_bytes = attempt_total;
            self.declared_routes = self.declared_routes.saturating_add(1);
        }
        let Some(completed) = delta.exact_completed_bytes else {
            return;
        };
        let (Some(route_completed), Some(attempt_completed)) = (
            self.current_route_completed.checked_add(completed),
            self.completed_bytes.checked_add(completed),
        ) else {
            self.invalid = true;
            return;
        };
        self.current_route_completed = route_completed;
        self.completed_bytes = attempt_completed;
        if self.current_route_total.is_none()
            || self
                .current_route_total
                .is_some_and(|total| route_completed > total)
            || attempt_completed > self.total_bytes
        {
            self.invalid = true;
        }
    }

    /// Returns whether the route is terminal for this attempt and can remain
    /// in the completed side of a later exact ETA basis.
    pub(super) fn finish_route(&mut self, succeeded: bool) -> bool {
        if self.invalid {
            return false;
        }
        if let Some(total) = self.current_route_total {
            if total != self.current_route_completed {
                self.invalid = true;
                return false;
            }
            return !succeeded;
        }
        let Some(total_bytes) = self
            .total_bytes
            .checked_add(self.current_route_observed_bytes)
        else {
            self.invalid = true;
            return false;
        };
        let Some(completed_bytes) = self
            .completed_bytes
            .checked_add(self.current_route_observed_bytes)
        else {
            self.invalid = true;
            return false;
        };
        let Some(completed_routes) = self.completed_unaccounted_routes.checked_add(1) else {
            self.invalid = true;
            return false;
        };
        self.total_bytes = total_bytes;
        self.completed_bytes = completed_bytes;
        self.completed_unaccounted_routes = completed_routes;
        true
    }

    fn all_route_work_is_known(&self, expected_routes: usize) -> bool {
        self.declared_routes
            .checked_add(self.completed_unaccounted_routes)
            == Some(expected_routes)
    }

    pub(super) fn snapshot(&self, expected_routes: usize) -> Option<SourceBackedExactScanProgress> {
        (!self.invalid
            && expected_routes != 0
            && self.all_route_work_is_known(expected_routes)
            && self.completed_bytes <= self.total_bytes)
            .then_some(SourceBackedExactScanProgress {
                total_bytes: self.total_bytes,
                completed_bytes: self.completed_bytes,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(total_bytes: Option<u64>, completed_bytes: u64) -> SourceBackedRecordProgressDelta {
        SourceBackedRecordProgressDelta {
            exact_total_bytes: total_bytes,
            exact_completed_bytes: Some(completed_bytes),
            ..Default::default()
        }
    }

    fn unaccounted_delta(completed_bytes: u64) -> SourceBackedRecordProgressDelta {
        SourceBackedRecordProgressDelta {
            completed_bytes,
            ..Default::default()
        }
    }

    fn complete_two_routes() -> AttemptExactScanAccounting {
        let mut accounting = AttemptExactScanAccounting::default();
        accounting.begin_route();
        accounting.observe(&delta(Some(100), 100));
        accounting.finish_route(true);
        accounting.begin_route();
        accounting.observe(&delta(Some(200), 50));
        assert_eq!(
            accounting.snapshot(2),
            Some(SourceBackedExactScanProgress {
                total_bytes: 300,
                completed_bytes: 150,
            })
        );
        accounting.observe(&delta(None, 150));
        accounting.finish_route(true);
        accounting
    }

    #[test]
    fn exact_multi_route_accounting_aggregates_without_route_buckets() {
        assert_eq!(
            complete_two_routes().snapshot(2),
            Some(SourceBackedExactScanProgress {
                total_bytes: 300,
                completed_bytes: 300,
            })
        );
    }

    #[test]
    fn exact_empty_route_preserves_later_nonempty_route_accounting() {
        let mut accounting = AttemptExactScanAccounting::default();
        accounting.begin_route();
        accounting.observe(&delta(Some(0), 0));
        accounting.finish_route(true);
        accounting.begin_route();
        accounting.observe(&delta(Some(200), 50));

        assert_eq!(
            accounting.snapshot(2),
            Some(SourceBackedExactScanProgress {
                total_bytes: 200,
                completed_bytes: 50,
            })
        );
    }

    #[test]
    fn completed_unaccounted_route_preserves_exact_final_route_eta_basis() {
        let mut accounting = AttemptExactScanAccounting::default();
        accounting.begin_route();
        accounting.observe(&unaccounted_delta(25));
        assert_eq!(accounting.snapshot(2), None);
        assert!(accounting.finish_route(true));
        assert_eq!(accounting.snapshot(2), None);

        accounting.begin_route();
        accounting.observe(&delta(Some(200), 50));
        assert_eq!(
            accounting.snapshot(2),
            Some(SourceBackedExactScanProgress {
                total_bytes: 225,
                completed_bytes: 75,
            })
        );
    }

    #[test]
    fn tolerated_terminal_failure_is_accounted_but_partial_failure_and_overflow_revoke() {
        let mut failed = AttemptExactScanAccounting::default();
        failed.begin_route();
        failed.observe(&unaccounted_delta(25));
        assert!(failed.finish_route(false));
        assert_eq!(
            failed.snapshot(1),
            Some(SourceBackedExactScanProgress {
                total_bytes: 25,
                completed_bytes: 25,
            })
        );

        let mut exact_terminal_failure = AttemptExactScanAccounting::default();
        exact_terminal_failure.begin_route();
        exact_terminal_failure.observe(&delta(Some(0), 0));
        assert!(exact_terminal_failure.finish_route(false));
        exact_terminal_failure.begin_route();
        exact_terminal_failure.observe(&delta(Some(200), 50));
        assert_eq!(
            exact_terminal_failure.snapshot(2),
            Some(SourceBackedExactScanProgress {
                total_bytes: 200,
                completed_bytes: 50,
            })
        );

        let mut exact_failure = AttemptExactScanAccounting::default();
        exact_failure.begin_route();
        exact_failure.observe(&delta(Some(100), 25));
        assert!(!exact_failure.finish_route(false));
        assert_eq!(exact_failure.snapshot(1), None);

        let mut overflowed = AttemptExactScanAccounting::default();
        overflowed.begin_route();
        overflowed.observe(&unaccounted_delta(u64::MAX));
        overflowed.observe(&unaccounted_delta(1));
        assert!(!overflowed.finish_route(true));
        assert_eq!(overflowed.snapshot(1), None);
    }

    #[test]
    fn terminal_mutation_rollback_and_certified_missing_paths_revoke_accounting() {
        for path in [
            "terminal source mutation",
            "route rollback",
            "certified-missing mutation",
        ] {
            let mut accounting = complete_two_routes();
            accounting.revoke();
            assert_eq!(accounting.snapshot(2), None, "{path}");
        }
    }
}
