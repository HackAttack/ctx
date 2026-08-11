use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::progress::ProgressArg;
use crate::ui::Ui;
use crate::ImportArgs;

mod automatic_source_refresh;
mod catalog;
mod core_refresh;
mod entry;
mod explicit;
mod explicit_source_catalog;
mod history_source_plugin;
mod provider_refresh;
mod report;
mod totals;

use automatic_source_refresh::{
    run_automatic_source_refresh_import, AutomaticSourceRefreshImportContext,
};
pub(crate) use ctx_history_ingest_application::SourceStats;
pub(crate) use entry::{import_report_analytics_outcome, import_report_failure_type, run_import};
use explicit::{run_explicit_source_catalog_import, ExplicitSourceCatalogImportContext};
#[cfg(test)]
pub(crate) use explicit_source_catalog::load_explicit_source_catalog_authority;
pub(crate) use explicit_source_catalog::{
    explicit_source_for_import, relocate_explicit_source, relocation_authority_for_import,
    upsert_explicit_source, ExplicitSourceCatalogAuthority,
};
use history_source_plugin::{run_history_source_plugin_import, HistorySourcePluginImportContext};
pub(crate) use provider_refresh::{ProviderRefreshCollector, ProviderRefreshRuntimeFacts};
pub(crate) use totals::ImportTotals;

#[derive(Debug)]
pub(crate) struct ImportReport {
    pub(crate) resume: bool,
    pub(crate) totals: ImportTotals,
    pub(crate) sources: Vec<Value>,
}

impl ImportReport {
    pub(crate) fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ImportRunOptions {
    pub(crate) progress: ProgressArg,
    pub(crate) json: bool,
    pub(crate) operation: &'static str,
}

pub(crate) struct ImportRunPresentation<'a> {
    pub(crate) options: ImportRunOptions,
    pub(crate) ui: &'a mut Ui,
}

pub(crate) fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

pub(crate) fn run_import_internal(
    args: &ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    config: &crate::config::AppConfig,
    presentation: ImportRunPresentation<'_>,
) -> Result<ImportReport> {
    let ImportRunPresentation { options, ui } = presentation;
    match validated_route(args)? {
        ctx_history_ingest_application::IngestRoute::HistorySourcePlugin => {
            run_history_source_plugin_import(HistorySourcePluginImportContext {
                args,
                data_root,
                telemetry,
                provider_refreshes,
                refresh_trigger,
                config,
                options,
                ui,
            })
        }
        ctx_history_ingest_application::IngestRoute::ExplicitPath => {
            run_explicit_source_catalog_import(ExplicitSourceCatalogImportContext {
                args,
                data_root,
                telemetry,
                provider_refreshes,
                refresh_trigger,
                config,
                options,
                ui,
            })
        }
        ctx_history_ingest_application::IngestRoute::Automatic => {
            run_automatic_source_refresh_import(AutomaticSourceRefreshImportContext {
                args,
                data_root,
                provider_refreshes,
                config,
                options,
                ui,
            })
        }
    }
}

fn validated_route(args: &ImportArgs) -> Result<ctx_history_ingest_application::IngestRoute> {
    ctx_history_ingest_application::validate_ingest_request(
        &ctx_history_ingest_application::IngestRequest {
            path: args.path.clone(),
            provider: args.provider.map(|provider| provider.capture_provider()),
            custom_jsonl: args.input_format.is_some(),
            history_source: args.history_source.clone(),
            history_source_manifests: args.history_source_manifest.clone(),
            all: args.all,
        },
    )
}
