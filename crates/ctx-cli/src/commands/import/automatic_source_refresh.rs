use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ctx_history_capture::{
    discover_provider_sources, source_backed_source_failure_identity, DiscoveryIssueKind,
    DiscoveryReport, ProviderImportWorkResult, ProviderSource, ProviderSourceStatus,
    HERMES_STATE_DB_UNSUPPORTED_REASON,
};
use ctx_history_core::platform_security::establish_private_data_root;
use serde_json::json;

use crate::{
    analytics::ProviderRefreshTrigger,
    compact_json,
    progress::ProgressReporter,
    provider_sources::{discovered_sources_for_provider_report, manual_path_guidance},
    ImportArgs,
};

use super::{
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    ImportReport, ImportRunOptions, ImportTotals, ProviderRefreshCollector,
};

const MAX_REPORTED_SOURCE_FAILURES: usize = 3;

struct AutomaticSourceDiscovery<'a> {
    home: &'a std::path::Path,
}

impl ctx_history_ingest_application::SourceDiscoveryPort for AutomaticSourceDiscovery<'_> {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        Ok(DiscoveryReport {
            sources: discover_provider_sources(self.home),
            issues: Vec::new(),
        })
    }

    fn discover_provider(
        &self,
        provider: ctx_history_core::CaptureProvider,
    ) -> Result<DiscoveryReport> {
        let mut report = self.discover_all()?;
        report.sources.retain(|source| source.provider == provider);
        Ok(report)
    }
}

fn published_request_scanned_routes(scanned_routes: Option<usize>) -> Result<usize> {
    scanned_routes.context("published daemon source refresh omitted its scanned route count")
}

pub(super) struct AutomaticSourceRefreshImportContext<'a> {
    pub(super) args: &'a ImportArgs,
    pub(super) data_root: PathBuf,
    pub(super) provider_refreshes: &'a mut ProviderRefreshCollector,
    pub(super) config: &'a crate::config::AppConfig,
    pub(super) options: ImportRunOptions,
    pub(super) ui: &'a mut crate::ui::Ui,
}

pub(super) fn run_automatic_source_refresh_import(
    context: AutomaticSourceRefreshImportContext<'_>,
) -> Result<ImportReport> {
    if context.args.history_source.is_some() || !context.args.history_source_manifest.is_empty() {
        bail!(
            "history-source plugins without a source-backed adapter are not supported in the v0.26 history epoch; import an approved provider source or explicit JSONL path"
        );
    }
    validate_selected_provider(context.args)?;

    let mut progress = ProgressReporter::new(
        context.ui,
        context.options.progress,
        context.options.json,
        context.options.operation,
        0,
    );
    let home = crate::identity::home_dir()
        .context("resolve user home for provider-root safety preflight")?;
    let preflight = ctx_history_ingest_application::automatic_source_preflight(
        &AutomaticSourceDiscovery { home: &home },
        &context.data_root,
    )
    .context("validate provider roots before initializing ctx state")?;
    let hermes = preflight.hermes_only_candidate.as_ref();
    let has_importable_source = preflight.has_importable_source;
    let data_root_exists = context
        .data_root
        .try_exists()
        .context("inspect ctx data root before unsupported-source reporting")?;
    if let Some(hermes) = hermes.filter(|_| !has_importable_source && !data_root_exists) {
        // A cold Hermes-only invocation cannot publish a generation. Return
        // the same typed source evidence as explicit selection without
        // creating ctx state or dispatching a provider parser.
        return unsupported_source_import_report(context.args.resume, hermes);
    }
    establish_private_data_root(&context.data_root)
        .context("protect ctx data root before provider refresh")?;
    let refresh = wait_for_import_core_refresh(
        &context.data_root,
        context.config,
        context.args.no_daemon,
        ImportCoreRefreshRequest::Automatic,
        &mut progress,
    )?;
    let receipt = refresh
        .receipt
        .clone()
        .context("daemon source refresh published without an authoritative terminal receipt")?;
    let request_previous_generation = refresh.request_previous_generation.clone();
    let request_generation_changed = refresh.request_generation_changed;
    let scanned_routes = published_request_scanned_routes(refresh.scanned_routes)?;
    let request_id = refresh.request_id.clone();
    let index = refresh.pin.into_index();
    let manifest = index.manifest();
    let current = receipt.current;
    let sources_completed_with_rejections = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.is_success() && result.rejected_record_total != 0)
        .count();
    let totals = ImportTotals {
        // Core receipts describe the committed current generation, not
        // synthetic per-run session/event/file totals.
        per_run_counts_available: false,
        terminal_route_counts_available: true,
        // Route-result counts are reported separately from per-run import
        // counts because the receipt certifies a whole Core generation.
        failed_sources: receipt.source_failure_total(),
        sources_completed_with_rejections,
        failed: usize::try_from(receipt.rejected_record_total()).unwrap_or(usize::MAX),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if request_generation_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };
    context.provider_refreshes.record_core_publication(
        ProviderRefreshTrigger::Import,
        request_generation_changed,
        receipt.source_failure_total(),
        receipt.rejected_record_total(),
    );

    let mut report_sources = vec![compact_json(json!({
        "status": if receipt.source_failure_total() != 0 || receipt.rejected_record_total() != 0 {
            "partial"
        } else {
            "published"
        },
        "failure_scope": match (
            receipt.source_failure_total() != 0,
            receipt.rejected_record_total() != 0,
        ) {
            (false, false) => "none",
            (false, true) => "record",
            (true, false) => "source",
            (true, true) => "record_and_source",
        },
        "failure_type": match (
            receipt.source_failure_total() != 0,
            receipt.rejected_record_total() != 0,
        ) {
            (false, false) => "none",
            (false, true) => "record_rejection",
            (true, false) => "source_failure",
            (true, true) => "record_rejection_and_source_failure",
        },
        "outcome": receipt.terminal_outcome(),
        "source_format": "provider_authoritative_all",
        "change": if request_generation_changed { "changed" } else { "no_op" },
        "previous_generation": request_previous_generation,
        "published_generation": receipt.published_generation,
        "generation_changed": request_generation_changed,
        "scanned_routes": scanned_routes,
        "successful_routes": receipt.successful_route_total(),
        "source_failure_total": receipt.source_failure_total(),
        "source_failures_omitted": receipt.source_failures_omitted()
            .saturating_add(receipt.source_failure_diagnostic_count()
                .saturating_sub(MAX_REPORTED_SOURCE_FAILURES)),
        "rejected_record_total": receipt.rejected_record_total(),
        "rejected_records": receipt.rejected_record_total(),
        "sources_completed_with_rejections": sources_completed_with_rejections,
        "rejections": {
            "rejected_records": receipt.rejected_record_total(),
            "sources_completed_with_rejections": sources_completed_with_rejections,
            "diagnostics_reported": receipt.rejection_diagnostic_count()
                .min(MAX_REPORTED_SOURCE_FAILURES),
            "diagnostics_omitted": receipt.rejection_diagnostics_omitted()
                .saturating_add(receipt.rejection_diagnostic_count()
                    .saturating_sub(MAX_REPORTED_SOURCE_FAILURES) as u64),
        },
        "rejection_diagnostics_omitted": receipt.rejection_diagnostics_omitted()
            .saturating_add(receipt.rejection_diagnostic_count()
                .saturating_sub(MAX_REPORTED_SOURCE_FAILURES) as u64),
        "current_source_count": current.source_count,
        "current_indexed_documents": current.indexed_documents,
        "current_complete_records": current.complete_records,
        "current_retained_records": current.retained_records,
        "current_rejected_records": current.rejected_records,
        "current_ignored_records": current.ignored_records,
        "current_certified_source_bytes": current.certified_source_bytes,
        "current_sources_with_rejections": current.sources_with_rejections,
        "removed_source_count": current.removed_source_count,
        "policy_schema_hash": manifest.policy_schema_hash.clone(),
        "certified_source_count": current.source_count,
        "certified_source_bytes": current.certified_source_bytes,
        "daemon_request_id": request_id,
        "daemon_request_metadata": {
            "owner": "daemon",
            "operation": "import",
            "trigger": "import",
            "trigger_provenance": "automatic_provider_refresh",
        },
    }))];
    report_sources.extend(
        receipt
            .source_failures()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .map(|failure| {
                source_failure_report_row(
                    &failure.source_identity,
                    &failure.provider,
                    &failure.class,
                    failure.carried_forward,
                    &failure.source_selector,
                    &failure.detail,
                )
            }),
    );
    report_sources.extend(
        receipt
            .rejection_diagnostics()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .map(|rejection| {
                compact_json(json!({
                    "status": "rejection",
                    "failure_scope": "record",
                    "failure_type": "record_rejection",
                    "source_identity": rejection.source_identity,
                    "provider": rejection.provider,
                    "source_selector": rejection.source_selector,
                    "line": rejection.line,
                    "payload_type": rejection.payload_type,
                    "detail": rejection.detail,
                    "error": rejection.detail,
                    "source_files": 0,
                    "source_bytes": 0,
                }))
            }),
    );

    Ok(ImportReport {
        resume: context.args.resume,
        totals,
        sources: report_sources,
    })
}

pub(super) fn source_failure_report_row(
    source_identity: &str,
    provider: &str,
    class: &str,
    carried_forward: bool,
    source_selector: &str,
    detail: &str,
) -> serde_json::Value {
    let failure_type = if class == "incompatible" {
        "unsupported_schema"
    } else {
        "other"
    };
    compact_json(json!({
        "status": "failure",
        "failure_scope": "source",
        "failure_type": failure_type,
        "source_identity": source_identity,
        "provider": provider,
        "source_failure_class": class,
        "carried_forward": carried_forward,
        "source_selector": source_selector,
        "detail": detail,
        "error": detail,
        "source_files": 0,
        "source_bytes": 0,
    }))
}

pub(super) fn unsupported_source_import_report(
    resume: bool,
    source: &ProviderSource,
) -> Result<ImportReport> {
    let detail = source
        .unsupported_reason
        .unwrap_or("the selected provider source is unsupported");
    let source_identity = source_backed_source_failure_identity(source)
        .map_err(anyhow::Error::from)
        .context("derive unsupported source identity")?;
    Ok(ImportReport {
        resume,
        totals: ImportTotals {
            terminal_route_counts_available: true,
            failed_sources: 1,
            work_result: ProviderImportWorkResult::NoOp,
            ..ImportTotals::default()
        },
        sources: vec![source_failure_report_row(
            &source_identity,
            source.provider.as_str(),
            "incompatible",
            false,
            &source.path.display().to_string(),
            detail,
        )],
    })
}

fn validate_selected_provider(args: &ImportArgs) -> Result<()> {
    let Some(provider) = args.provider.map(|provider| provider.capture_provider()) else {
        return Ok(());
    };
    let report = discovered_sources_for_provider_report(provider);
    if report.sources.iter().any(|source| {
        source.status == ProviderSourceStatus::Available && source.import_support.is_importable()
    }) {
        return Ok(());
    }
    let provider_name = crate::provider_sources::provider_cli_name(provider);
    if provider == ctx_history_core::CaptureProvider::Hermes {
        if report.sources.iter().any(|source| source.exists) {
            // Let the provider refresh lifecycle emit the canonical
            // source-scoped unsupported_schema row. This is not an import
            // override and does not dispatch a parser.
            return Ok(());
        }
        bail!(
            "Hermes import is unavailable in this ctx release: {HERMES_STATE_DB_UNSUPPORTED_REASON}"
        );
    }
    let guidance = manual_path_guidance(provider);
    if let Some(source) = report
        .sources
        .iter()
        .find(|source| source.status == ProviderSourceStatus::Unsupported)
    {
        bail!(
            "detected unsupported history at {}; current ctx cannot import that path for {provider_name}; use `{guidance}`",
            source.path.display()
        );
    }
    if let Some(issue) = report.issues.first() {
        let summary = match issue.kind {
            DiscoveryIssueKind::NoDiskHistory => {
                format!("{provider_name} has no disk history selected")
            }
            DiscoveryIssueKind::SelectorUnreconstructible => {
                format!("{provider_name} automatic history location cannot be safely reconstructed")
            }
            DiscoveryIssueKind::InsufficientOfficialEvidence => {
                format!("{provider_name} has no official automatic history location established")
            }
        };
        bail!("{summary}: {}; use `{guidance}`", issue.reason);
    }
    bail!(
        "no importable {provider_name} history source was discovered; use `{guidance}` to select one"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_import_has_no_legacy_history_store_dependency() {
        let source = include_str!("automatic_source_refresh.rs");
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["Store", "::open"].concat(),
            ["work", ".sqlite"].concat(),
            ["projection_", "journal"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "automatic source refresh contains forbidden legacy dependency `{forbidden}`"
            );
        }
    }

    #[test]
    fn logical_publication_reports_zero_scans_without_receipt_count_fallback() {
        assert_eq!(published_request_scanned_routes(Some(0)).unwrap(), 0);
        assert!(published_request_scanned_routes(None).is_err());
    }
}
