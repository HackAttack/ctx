use std::path::PathBuf;

use anyhow::Result;
use ctx_history_ingest_application::{IngestReport, ProviderRefreshModeFact};

use crate::analytics::{
    bytes_bucket, count_bucket, ImportFailureScope as AnalyticsImportFailureScope,
    ImportFailureType as AnalyticsImportFailureType, ImportOutcome as AnalyticsImportOutcome,
    ImportTelemetry, ProviderRefreshSourceMode, ProviderRefreshTrigger,
};
use crate::ui::Ui;
use crate::ImportArgs;

use super::{
    application_adapter::CliImportHost,
    provider_refresh::{ProviderRefreshCollector, ProviderRefreshRuntimeFacts},
};

pub(crate) fn run_import(
    args: ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    config: &crate::config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    let json = args.format.is_json();
    if args.partial && !json {
        let document =
            ctx_cli_presentation::commands::render_partial_deprecation(ui.stderr_context());
        ui.write_stderr(&document)?;
    }
    let request = import_request(&args);
    provider_refreshes.start_timing();
    let mut host = CliImportHost::new(config);
    let config_snapshot = crate::history_config::CliHistoryConfigSnapshot::new(config);
    let report = ctx_history_cli::run_import_application(
        request,
        &data_root,
        crate::identity::home_dir(),
        &config_snapshot,
        &mut host,
        ui,
    );
    provider_refreshes.stop_timing();
    let report = match report {
        Ok(report) => report,
        Err(err) => {
            insert_import_error_analytics(telemetry, &err);
            return Err(err);
        }
    };
    record_application_facts(
        &report,
        telemetry,
        provider_refreshes,
        ProviderRefreshTrigger::Import,
    );
    insert_import_report_analytics(telemetry, &report);
    if json {
        crate::output::print_json(ctx_history_cli::import_report_json(&report))?;
    } else {
        let document = ctx_history_cli::render_import_report_human(ui.stdout_context(), &report);
        ui.write_stdout(&document)?;
    }
    if let Some(error) = ctx_history_cli::import_completion_error(&report) {
        return Err(error);
    }
    Ok(())
}

pub(crate) fn insert_import_report_analytics(
    telemetry: &mut ImportTelemetry,
    report: &IngestReport,
) {
    let (outcome, failure_scope) = ctx_history_cli::import_report_outcome(&report.totals);
    telemetry.outcome = Some(match outcome {
        "success" => AnalyticsImportOutcome::Success,
        "failure" => AnalyticsImportOutcome::Failure,
        "completed_with_rejections" => AnalyticsImportOutcome::CompletedWithRejections,
        "completed_with_source_failures" => AnalyticsImportOutcome::CompletedWithSourceFailures,
        _ => AnalyticsImportOutcome::CompletedWithRejectionsAndSourceFailures,
    });
    telemetry.failure_scope = Some(match failure_scope {
        "none" => AnalyticsImportFailureScope::None,
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        _ => AnalyticsImportFailureScope::RecordAndSource,
    });
    telemetry.failure_type = Some(
        match ctx_history_cli::import_report_failure_type(&report.totals) {
            "none" => AnalyticsImportFailureType::None,
            "record_rejection" => AnalyticsImportFailureType::RecordRejection,
            "source_failure" => AnalyticsImportFailureType::SourceFailure,
            _ => AnalyticsImportFailureType::RecordRejectionAndSourceFailure,
        },
    );
}

pub(crate) fn insert_import_error_analytics(
    telemetry: &mut ImportTelemetry,
    error: &anyhow::Error,
) {
    telemetry.outcome = Some(AnalyticsImportOutcome::Failure);
    telemetry.failure_scope = Some(match ctx_history_cli::import_error_scope(error).as_str() {
        "record" => AnalyticsImportFailureScope::Record,
        "source" => AnalyticsImportFailureScope::Source,
        "record_and_source" => AnalyticsImportFailureScope::RecordAndSource,
        _ => AnalyticsImportFailureScope::Invocation,
    });
    telemetry.failure_type = Some(match ctx_history_cli::import_failure_type(error).as_str() {
        "invalid_request" => AnalyticsImportFailureType::InvalidRequest,
        "io" => AnalyticsImportFailureType::Io,
        _ => AnalyticsImportFailureType::Other,
    });
}

fn import_request(args: &ImportArgs) -> ctx_history_cli::ImportRequest {
    ctx_history_cli::ImportRequest {
        provider: args
            .provider
            .map(|provider| provider.capture_provider().into()),
        path: args.path.clone(),
        relocate_from: args.relocate_from.clone(),
        history_source: args.history_source.clone(),
        history_source_manifests: args.history_source_manifest.clone(),
        reset_cursor: args.reset_cursor,
        input_format: args
            .input_format
            .map(|_| ctx_history_cli::ImportFormat::CtxHistoryJsonlV1),
        all: args.all,
        resume: args.resume,
        partial: args.partial,
        no_daemon: args.no_daemon,
        format: if args.format.is_json() {
            ctx_history_cli::OutputFormat::Json
        } else {
            ctx_history_cli::OutputFormat::Text
        },
        progress: match args.progress {
            crate::progress::ProgressArg::Auto => ctx_history_cli::ProgressMode::Auto,
            crate::progress::ProgressArg::Plain => ctx_history_cli::ProgressMode::Plain,
            crate::progress::ProgressArg::Json => ctx_history_cli::ProgressMode::Json,
            crate::progress::ProgressArg::None => ctx_history_cli::ProgressMode::None,
        },
    }
}

fn record_application_facts(
    outcome: &IngestReport,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
) {
    if let Some(facts) = outcome.telemetry {
        telemetry.sources_seen = Some(count_bucket(facts.sources_seen));
        telemetry.source_files = Some(count_bucket(facts.source_files));
        telemetry.source_bytes = Some(bytes_bucket(facts.source_bytes));
        telemetry.failed_sources = Some(count_bucket(facts.failed_sources));
        telemetry.sessions_imported = None;
        telemetry.events_imported = None;
        telemetry.edges_imported = None;
        telemetry.skipped = None;
        telemetry.rejected_records = None;
    }
    if let Some(facts) = outcome.provider_refresh.as_ref() {
        provider_refreshes.record_success_with_facts(
            facts.provider,
            refresh_trigger,
            match facts.mode {
                ProviderRefreshModeFact::ExplicitPath => ProviderRefreshSourceMode::ExplicitPath,
                ProviderRefreshModeFact::ExplicitFormat => {
                    ProviderRefreshSourceMode::ExplicitFormat
                }
                ProviderRefreshModeFact::HistorySourcePlugin => {
                    ProviderRefreshSourceMode::HistorySourcePlugin
                }
            },
            &facts.summary,
            &facts.stats,
            ProviderRefreshRuntimeFacts::observed_success(facts.duration, &facts.summary),
        );
    }
    if let Some(facts) = outcome.core_publication {
        provider_refreshes.record_core_publication(
            ProviderRefreshTrigger::Import,
            facts.generation_changed,
            facts.source_failure_total,
            facts.rejected_record_total,
        );
    }
}
