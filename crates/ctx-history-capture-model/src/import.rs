use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};

/// Provider-local capture context shared by isolated adapter crates.
#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub source_root: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
}

impl ProviderAdapterContext {
    pub fn source_root_display(&self) -> Option<String> {
        self.source_root
            .as_ref()
            .or(self.source_path.as_ref())
            .map(|path| path.display().to_string())
    }
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            source_root: None,
            imported_at: utc_now(),
        }
    }
}

pub fn default_machine_id() -> String {
    std::env::var("CTX_MACHINE_ID")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    #[serde(skip)]
    pub work_remaining: bool,
    pub failures: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderImportWorkResult {
    Changed,
    #[default]
    NoOp,
}

impl ProviderImportWorkResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::NoOp => "no_op",
        }
    }

    pub fn merge(self, other: Self) -> Self {
        if self == Self::Changed || other == Self::Changed {
            Self::Changed
        } else {
            Self::NoOp
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportFailure {
    pub line: usize,
    pub error: String,
}

pub fn push_provider_import_failure(
    summary: &mut ProviderImportSummary,
    line: usize,
    error: String,
) {
    summary.failed += 1;
    summary.failures.push(ProviderImportFailure { line, error });
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSummary {
    pub source_files: usize,
    pub source_bytes: u64,
    pub cataloged_sessions: usize,
    pub cached_sessions: usize,
    pub parsed_sessions: usize,
    pub skipped_sessions: usize,
    pub failed_sessions: usize,
    pub failures: Vec<ProviderImportFailure>,
}

impl ProviderImportSummary {
    pub fn has_accepted_content(&self) -> bool {
        self.imported_events > 0 || self.imported_edges > 0
    }

    pub fn work_result(&self) -> ProviderImportWorkResult {
        if self.imported > 0 || (self.skipped == 0 && self.has_accepted_content()) {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_summary_wire_shape_and_skip_semantics_are_stable() {
        let summary = ProviderImportSummary {
            imported: 1,
            skipped: 2,
            failed: 3,
            imported_sessions: 4,
            skipped_sessions: 5,
            imported_events: 6,
            skipped_events: 7,
            imported_edges: 8,
            skipped_edges: 9,
            work_remaining: true,
            failures: vec![ProviderImportFailure {
                line: 10,
                error: "invalid record".to_owned(),
            }],
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(
            json,
            r#"{"imported":1,"skipped":2,"failed":3,"imported_sessions":4,"skipped_sessions":5,"imported_events":6,"skipped_events":7,"imported_edges":8,"skipped_edges":9,"failures":[{"line":10,"error":"invalid record"}]}"#
        );
        let decoded: ProviderImportSummary = serde_json::from_str(&json).unwrap();
        assert!(!decoded.work_remaining);
        assert_eq!(decoded.work_result(), ProviderImportWorkResult::Changed);
    }

    #[test]
    fn catalog_summary_wire_shape_and_round_trip_are_stable() {
        let summary = CatalogSummary {
            source_files: 1,
            source_bytes: 2,
            cataloged_sessions: 3,
            cached_sessions: 4,
            parsed_sessions: 5,
            skipped_sessions: 6,
            failed_sessions: 7,
            failures: vec![ProviderImportFailure {
                line: 8,
                error: "catalog failure".to_owned(),
            }],
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(
            json,
            r#"{"source_files":1,"source_bytes":2,"cataloged_sessions":3,"cached_sessions":4,"parsed_sessions":5,"skipped_sessions":6,"failed_sessions":7,"failures":[{"line":8,"error":"catalog failure"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<CatalogSummary>(&json).unwrap(),
            summary
        );
    }

    #[test]
    fn import_failure_accumulator_preserves_count_and_insertion_order() {
        let mut summary = ProviderImportSummary::default();

        push_provider_import_failure(&mut summary, 7, "first".to_owned());
        push_provider_import_failure(&mut summary, 3, "second".to_owned());

        assert_eq!(summary.failed, 2);
        assert_eq!(
            summary.failures,
            vec![
                ProviderImportFailure {
                    line: 7,
                    error: "first".to_owned(),
                },
                ProviderImportFailure {
                    line: 3,
                    error: "second".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn work_result_strings_and_merge_are_stable() {
        assert_eq!(ProviderImportWorkResult::Changed.as_str(), "changed");
        assert_eq!(ProviderImportWorkResult::NoOp.as_str(), "no_op");
        assert_eq!(
            ProviderImportWorkResult::NoOp.merge(ProviderImportWorkResult::Changed),
            ProviderImportWorkResult::Changed
        );
    }
}
