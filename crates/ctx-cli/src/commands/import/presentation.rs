use ctx_history_ingest_application::{
    AutomaticPublicationOutcome, ExactPublicationOutcome, IngestSourceOutcome,
    PluginPublicationOutcome, RecordRejectionOutcome, SourceFailureOutcome,
};
use serde_json::{json, Value};

use crate::{compact_json, progress::format_bytes};

use super::ImportReport;

pub(super) fn application_import_report(
    report: ctx_history_ingest_application::IngestReport,
    operation: &'static str,
) -> ImportReport {
    let sources = report
        .sources
        .iter()
        .map(|source| source_json(source, operation))
        .collect();
    ImportReport {
        resume: report.resume,
        totals: report.totals,
        sources,
    }
}

fn source_json(source: &IngestSourceOutcome, operation: &'static str) -> Value {
    match source {
        IngestSourceOutcome::Automatic(outcome) => automatic_json(outcome),
        IngestSourceOutcome::Exact(outcome) => exact_json(outcome, operation),
        IngestSourceOutcome::Plugin(outcome) => plugin_json(outcome),
        IngestSourceOutcome::SourceFailure(outcome) => source_failure_json(outcome),
        IngestSourceOutcome::Rejection(outcome) => rejection_row_json(outcome),
    }
}

fn automatic_json(outcome: &AutomaticPublicationOutcome) -> Value {
    let current = outcome.current;
    compact_json(json!({
        "status": outcome.status.as_str(),
        "failure_scope": outcome.failure_scope.as_str(),
        "failure_type": outcome.failure_type.as_str(),
        "outcome": outcome.terminal_outcome.as_str(),
        "source_format": "provider_authoritative_all",
        "change": outcome.change.as_str(),
        "previous_generation": outcome.previous_generation,
        "published_generation": outcome.published_generation,
        "generation_changed": outcome.generation_changed,
        "scanned_routes": outcome.scanned_routes,
        "successful_routes": outcome.successful_routes,
        "source_failure_total": outcome.source_failure_total,
        "source_failures_omitted": outcome.source_failures_omitted,
        "rejected_record_total": outcome.rejected_record_total,
        "rejected_records": outcome.rejected_record_total,
        "sources_completed_with_rejections": outcome.sources_completed_with_rejections,
        "rejections": {
            "rejected_records": outcome.rejected_record_total,
            "sources_completed_with_rejections": outcome.sources_completed_with_rejections,
            "diagnostics_reported": outcome.rejection_diagnostics_reported,
            "diagnostics_omitted": outcome.rejection_diagnostics_omitted,
        },
        "rejection_diagnostics_omitted": outcome.rejection_diagnostics_omitted,
        "current_source_count": current.source_count,
        "current_indexed_documents": current.indexed_documents,
        "current_complete_records": current.complete_records,
        "current_retained_records": current.retained_records,
        "current_rejected_records": current.rejected_records,
        "current_ignored_records": current.ignored_records,
        "current_certified_source_bytes": current.certified_source_bytes,
        "current_sources_with_rejections": current.sources_with_rejections,
        "removed_source_count": current.removed_source_count,
        "policy_schema_hash": outcome.policy_schema_hash,
        "certified_source_count": current.source_count,
        "certified_source_bytes": current.certified_source_bytes,
        "daemon_request_id": outcome.request_id,
        "daemon_request_metadata": {
            "owner": "daemon",
            "operation": "import",
            "trigger": "import",
            "trigger_provenance": "automatic_provider_refresh",
        },
    }))
}

fn source_failure_json(outcome: &SourceFailureOutcome) -> Value {
    compact_json(json!({
        "status": outcome.status.as_str(),
        "failure_scope": outcome.failure_scope.as_str(),
        "failure_type": outcome.failure_type.as_str(),
        "source_identity": outcome.source_identity,
        "provider": outcome.provider,
        "source_failure_class": outcome.source_failure_class,
        "carried_forward": outcome.carried_forward,
        "source_selector": outcome.source_selector,
        "detail": outcome.detail,
        "error": outcome.detail,
        "source_files": 0,
        "source_bytes": 0,
    }))
}

fn rejection_row_json(outcome: &RecordRejectionOutcome) -> Value {
    compact_json(json!({
        "status": "rejection",
        "failure_scope": "record",
        "failure_type": "record_rejection",
        "source_identity": outcome.source_identity,
        "provider": outcome.provider,
        "source_selector": outcome.source_selector,
        "line": outcome.line,
        "payload_type": outcome.payload_type,
        "detail": outcome.detail,
        "error": outcome.detail,
        "source_files": 0,
        "source_bytes": 0,
    }))
}

fn exact_json(outcome: &ExactPublicationOutcome, operation: &'static str) -> Value {
    let current = outcome.current;
    let rejection_diagnostics = outcome
        .rejection_diagnostics
        .iter()
        .map(|rejection| {
            json!({
                "source_identity": rejection.source_identity,
                "provider": rejection.provider,
                "path": rejection.source_selector,
                "line": rejection.line,
                "payload_type": rejection.payload_type,
                "class": rejection.class,
                "detail": rejection.detail,
            })
        })
        .collect::<Vec<_>>();
    let mut report = compact_json(json!({
        "status": outcome.status.as_str(),
        "failure_scope": outcome.failure_scope.as_str(),
        "failure_type": outcome.failure_type.as_str(),
        "provider": outcome.provider.as_str(),
        "path": outcome.path,
        "source_format": outcome.source_format,
        "route_identity": outcome.route_identity,
        "source_files": outcome.stats.files,
        "source_bytes": outcome.stats.bytes,
        "catalog_lineage": outcome.catalog_lineage,
        "request_overlay": outcome.request_overlay.to_json(),
        "previous_generation": outcome.previous_generation,
        "published_generation": outcome.published_generation,
        "generation_changed": outcome.generation_changed,
        "scanned_routes": outcome.scanned_routes,
        "successful_routes": outcome.successful_routes,
        "source_failure_total": outcome.source_failure_total,
        "route_source_failure_total": outcome.route_source_failure_total,
        "rejected_record_total": outcome.rejected_record_total,
        "rejection_diagnostics": rejection_diagnostics,
        "daemon_request_id": outcome.request_id,
        "daemon_request_metadata": {
            "owner": "daemon",
            "operation": operation,
            "trigger": "import",
            "trigger_provenance": "explicit_source_catalog",
        },
        "change": outcome.change.as_str(),
        "current_source_count": current.source_count,
        "current_indexed_documents": current.indexed_documents,
        "current_complete_records": current.complete_records,
        "current_retained_records": current.retained_records,
        "current_rejected_records": current.rejected_records,
        "current_ignored_records": current.ignored_records,
        "current_certified_source_bytes": current.certified_source_bytes,
        "current_sources_with_rejections": current.sources_with_rejections,
        "removed_source_count": current.removed_source_count,
    }));
    if outcome.route_source_failure_total != 0 {
        let source_identity = outcome
            .requested_failure
            .as_ref()
            .map(|failure| failure.source_identity.as_str())
            .unwrap_or("unavailable_in_bounded_diagnostics");
        let source_selector = outcome
            .requested_failure
            .as_ref()
            .map(|failure| failure.source_selector.as_str())
            .unwrap_or("");
        let detail = outcome
            .requested_failure
            .as_ref()
            .map(|failure| failure.detail.as_str())
            .unwrap_or("source failure detail omitted from bounded diagnostics");
        let failure_fields = json!({
            "source_identity": source_identity,
            "source_selector": source_selector,
            "source_failure_class": outcome.requested_failure_class,
            "carried_forward": outcome
                .requested_failure
                .as_ref()
                .is_some_and(|failure| failure.carried_forward),
            "detail": detail,
            "error": detail,
        });
        let (Value::Object(report), Value::Object(failure_fields)) = (&mut report, failure_fields)
        else {
            unreachable!("explicit import report fields are JSON objects")
        };
        report.extend(failure_fields);
    }
    report
}

fn plugin_json(outcome: &PluginPublicationOutcome) -> Value {
    let current = outcome.current;
    let source = &outcome.plugin_source;
    let rejection_diagnostics = outcome
        .rejection_diagnostics
        .iter()
        .map(|rejection| {
            json!({
                "source_identity": rejection.source_identity,
                "provider": rejection.provider,
                "path": rejection.source_selector,
                "line": rejection.line,
                "payload_type": rejection.payload_type,
                "detail": rejection.detail,
            })
        })
        .collect::<Vec<_>>();
    compact_json(json!({
        "status": outcome.status.as_str(),
        "failure_scope": outcome.failure_scope.as_str(),
        "failure_type": outcome.failure_type.as_str(),
        "provider": ctx_history_core::CaptureProvider::Custom.as_str(),
        "kind": "history_source_plugin",
        "plugin": source.plugin_name,
        "history_source": source.history_source(),
        "plugin_source": source.label(),
        "provider_key": source.provider_key,
        "source_id": source.source_id,
        "source_format": source.source_format,
        "route_source_format": outcome.route_source.source_format,
        "path": outcome.route_source.path,
        "source_files": outcome.stats.files,
        "source_bytes": outcome.stats.bytes,
        "catalog_lineage": outcome.catalog_lineage,
        "catalog_authority": outcome.catalog_authority.to_json(),
        "previous_generation": outcome.previous_generation,
        "published_generation": outcome.published_generation,
        "generation_changed": outcome.generation_changed,
        "rejected_record_total": outcome.rejected_record_total,
        "rejection_diagnostics": rejection_diagnostics,
        "daemon_request_id": outcome.request_id,
        "daemon_request_metadata": {
            "owner": "daemon",
            "trigger": "import",
            "trigger_provenance": "history_source_plugin",
        },
        "change": outcome.change.as_str(),
        "current_source_count": current.source_count,
        "current_indexed_documents": current.indexed_documents,
        "current_complete_records": current.complete_records,
        "current_retained_records": current.retained_records,
        "current_rejected_records": current.rejected_records,
        "current_ignored_records": current.ignored_records,
        "current_certified_source_bytes": current.certified_source_bytes,
        "current_sources_with_rejections": current.sources_with_rejections,
        "removed_source_count": current.removed_source_count,
        "provider_source_authority": true,
        "display_source_bytes": format_bytes(outcome.stats.bytes),
    }))
}
