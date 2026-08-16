#[cfg(test)]
use std::path::Path;

use serde::Serialize;

use super::store::{open_read_only, usage_store_exists};
use super::{
    estimate_usage, LocalUsageStorageAuthority, UsageControlSnapshot, UsageEstimates,
    DEFINITION_VERSION, RETENTION_DAYS, USAGE_REPORT_SCHEMA_VERSION,
};

mod query;
mod validation;

use query::query_report;
pub(super) use validation::{validate_rows, validate_rows_for_schema};

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub schema_version: i64,
    pub local_only: bool,
    pub read_only: bool,
    pub enabled: bool,
    pub state: &'static str,
    pub retention_days: i64,
    #[serde(skip)]
    pub definition_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definitions: Option<Vec<UsageDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimates: Option<UsageEstimates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<UsageReportError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageDefinition {
    pub definition_version: i64,
    pub ctx_versions: Vec<String>,
    pub first_day_utc: String,
    pub last_day_utc: String,
    pub active_days: u64,
    pub summary: UsageSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_operation: Vec<OperationSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub duration_buckets: Vec<DurationSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageSummary {
    pub calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,
    pub result_bearing_calls: u64,
    pub empty_calls: u64,
    pub not_applicable_calls: u64,
    pub result_count: u64,
    pub delivered_output_bytes: u64,
    pub delivered_context_bytes: u64,
    pub matched_normalized_session_bytes: u64,
    pub complete_context_eligible_calls: u64,
    pub unavailable_context_eligible_calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationSummary {
    pub ctx_version: String,
    pub surface: String,
    pub operation: String,
    pub calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,
    pub result_bearing_calls: u64,
    pub empty_calls: u64,
    pub not_applicable_calls: u64,
    pub result_count: u64,
    pub delivered_output_bytes: u64,
    pub delivered_context_bytes: u64,
    pub matched_normalized_session_bytes: u64,
    pub complete_context_eligible_calls: u64,
    pub unavailable_context_eligible_calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurationSummary {
    pub duration_bucket: String,
    pub calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReportError {
    pub code: &'static str,
    pub message: &'static str,
}

impl UsageReport {
    pub fn config_error() -> Self {
        error_report(
            false,
            "local_usage_config_unavailable",
            "local usage configuration could not be read",
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn ui_test_ready() -> Self {
        base_report(
            true,
            "ready",
            Some(vec![UsageDefinition {
                definition_version: DEFINITION_VERSION,
                ctx_versions: vec!["1.0.0".to_owned()],
                first_day_utc: "2026-07-29".to_owned(),
                last_day_utc: "2026-07-29".to_owned(),
                active_days: 1,
                summary: UsageSummary {
                    calls: 4,
                    successful_calls: 3,
                    failed_calls: 1,
                    result_bearing_calls: 1,
                    empty_calls: 2,
                    not_applicable_calls: 1,
                    result_count: 3,
                    ..UsageSummary::default()
                },
                by_operation: Vec::new(),
                duration_buckets: Vec::new(),
            }]),
            None,
            None,
        )
    }
}

pub fn read_report_authorized(
    authority: &LocalUsageStorageAuthority,
    control: &UsageControlSnapshot,
    detailed: bool,
) -> UsageReport {
    if !control.available() {
        return UsageReport::config_error();
    }
    if !control.enabled() {
        return base_report(false, "disabled", None, None, None);
    }
    let exists = match usage_store_exists(authority) {
        Ok(exists) => exists,
        Err(error) => {
            return error_report(true, "usage_store_unavailable", error.public_message());
        }
    };
    if !exists {
        return base_report(true, "empty", Some(Vec::new()), None, None);
    }
    match open_read_only(authority.database_path()).and_then(|mut store| {
        let (definitions, estimate_facts) = query_report(store.connection_mut(), detailed)?;
        let estimates = estimate_usage(estimate_facts)?;
        store.verify_unchanged()?;
        Ok((definitions, estimates))
    }) {
        Ok((definitions, estimates)) => {
            let state = if definitions.is_empty() {
                "empty"
            } else {
                "ready"
            };
            base_report(true, state, Some(definitions), estimates, None)
        }
        Err(error) => error_report(true, "usage_store_unavailable", error.public_message()),
    }
}

#[cfg(test)]
pub fn read_report(data_root: &Path, enabled: bool, detailed: bool) -> UsageReport {
    let authority =
        LocalUsageStorageAuthority::new(data_root.join(super::store::USAGE_FILE), "1.0.0");
    read_report_authorized(
        &authority,
        &UsageControlSnapshot::unversioned(enabled),
        detailed,
    )
}

fn base_report(
    enabled: bool,
    state: &'static str,
    definitions: Option<Vec<UsageDefinition>>,
    estimates: Option<UsageEstimates>,
    error: Option<UsageReportError>,
) -> UsageReport {
    UsageReport {
        schema_version: USAGE_REPORT_SCHEMA_VERSION,
        local_only: true,
        read_only: true,
        enabled,
        state,
        retention_days: RETENTION_DAYS,
        definition_version: DEFINITION_VERSION,
        definitions,
        estimates,
        error,
    }
}

fn error_report(enabled: bool, code: &'static str, message: &'static str) -> UsageReport {
    base_report(
        enabled,
        "error",
        None,
        None,
        Some(UsageReportError { code, message }),
    )
}
