use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::progress::ProgressArg;
use crate::ui::Ui;
use crate::ImportArgs;

mod application_adapter;
mod core_refresh;
mod entry;
mod explicit_source_catalog;
mod presentation;
mod provider_refresh;
mod report;
mod totals;

use application_adapter::{run_application_import, ApplicationImportContext};
pub(crate) use ctx_history_ingest_application::SourceStats;
pub(crate) use entry::{import_report_analytics_outcome, import_report_failure_type, run_import};
#[cfg(test)]
pub(crate) use explicit_source_catalog::load_explicit_source_catalog_authority;
pub(crate) use explicit_source_catalog::ExplicitSourceCatalogAuthority;
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
    run_application_import(ApplicationImportContext {
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
