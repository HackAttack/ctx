use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_history_capture::{source_backed_source_failure_identity, DiscoveryReport, ProviderSource};
use ctx_history_core::{platform_security::establish_private_data_root, CaptureProvider};
use ctx_history_ingest_application::{
    CaptureAdmissionPort, HistorySourcePluginSource, IngestProgressPort, IngestPublication,
    IngestRefreshPort, IngestReport, ProviderRefreshModeFact, ProviderSelectionGuidance,
    SourceDiscoveryPort, SourceStats,
};
use ctx_history_refresh::{ExplicitSourceCatalogAuthority, ExplicitSourceCatalogUpsert};

use crate::{
    analytics::{
        bytes_bucket, count_bucket, ImportTelemetry, ProviderRefreshSourceMode,
        ProviderRefreshTrigger,
    },
    history_source_plugins::prepare_source_backed_history_source,
    progress::{format_bytes, ProgressReporter},
    ImportArgs,
};

use super::{
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    explicit_source_catalog::{
        explicit_source_for_admission, relocate_explicit_source, relocation_authority_for_import,
        upsert_explicit_source,
    },
    ImportRunOptions, ProviderRefreshCollector, ProviderRefreshRuntimeFacts,
};

pub(super) struct ApplicationImportContext<'a> {
    pub args: &'a ImportArgs,
    pub data_root: PathBuf,
    pub telemetry: &'a mut ImportTelemetry,
    pub provider_refreshes: &'a mut ProviderRefreshCollector,
    pub refresh_trigger: ProviderRefreshTrigger,
    pub config: &'a crate::config::AppConfig,
    pub options: ImportRunOptions,
    pub ui: &'a mut crate::ui::Ui,
}

pub(super) fn run_application_import(
    context: ApplicationImportContext<'_>,
) -> Result<IngestReport> {
    let request = ctx_history_ingest_application::IngestRequest {
        path: context.args.path.clone(),
        provider: context
            .args
            .provider
            .map(|provider| provider.capture_provider()),
        custom_jsonl: context.args.input_format.is_some(),
        history_source: context.args.history_source.clone(),
        history_source_manifests: context.args.history_source_manifest.clone(),
        all: context.args.all,
        resume: context.args.resume,
        relocate_from: context.args.relocate_from.clone(),
        reset_cursor: context.args.reset_cursor,
        no_daemon: context.args.no_daemon,
    };
    let mut host = CliIngestHost {
        home: crate::identity::home_dir(),
        config: context.config,
        options: context.options,
        ui: Some(context.ui),
        progress: None,
    };
    let outcome =
        ctx_history_ingest_application::run_ingest(&request, &context.data_root, &mut host)?;
    record_application_facts(
        &outcome,
        context.telemetry,
        context.provider_refreshes,
        context.refresh_trigger,
    );
    Ok(outcome)
}

struct CliIngestHost<'a> {
    home: Option<PathBuf>,
    config: &'a crate::config::AppConfig,
    options: ImportRunOptions,
    ui: Option<&'a mut crate::ui::Ui>,
    progress: Option<ProgressReporter<'a>>,
}

impl SourceDiscoveryPort for CliIngestHost<'_> {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        let home = self
            .home
            .as_deref()
            .context("resolve user home for provider-root safety preflight")?;
        Ok(ctx_history_cli::discovered_sources_report(Some(home)))
    }

    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport> {
        Ok(ctx_history_cli::discovered_sources_for_provider_report(
            self.home.as_deref(),
            provider,
        ))
    }

    fn provider_selection_guidance(&self, provider: CaptureProvider) -> ProviderSelectionGuidance {
        ctx_history_cli::provider_selection_guidance(provider)
    }
}

impl CaptureAdmissionPort for CliIngestHost<'_> {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()> {
        establish_private_data_root(data_root).map_err(anyhow::Error::new)
    }

    fn explicit_source(
        &self,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource> {
        explicit_source_for_admission(path, provider, custom_jsonl)
    }

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource> {
        Ok(
            prepare_source_backed_history_source(source.clone(), reset_cursor)?
                .provider_source()
                .clone(),
        )
    }

    fn admit_exact(
        &mut self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert> {
        if let Some(old_path) = relocate_from {
            let relocation = relocation_authority_for_import(data_root, old_path)?;
            relocate_explicit_source(data_root, source, relocation)
        } else {
            upsert_explicit_source(data_root, source)
        }
    }

    fn source_failure_identity(&self, source: &ProviderSource) -> Result<String> {
        source_backed_source_failure_identity(source).map_err(anyhow::Error::from)
    }
}

impl IngestProgressPort for CliIngestHost<'_> {
    fn begin(&mut self, total_bytes: u64) -> Result<()> {
        let ui = self
            .ui
            .take()
            .context("ingest progress was initialized more than once")?;
        self.progress = Some(ProgressReporter::new(
            ui,
            self.options.progress,
            self.options.json,
            self.options.operation,
            total_bytes,
        ));
        Ok(())
    }

    fn catalog_exact(&mut self, source: &ProviderSource, stats: SourceStats) -> Result<()> {
        self.progress_mut()?.message(
            "cataloging",
            format!(
                "Cataloging {} source {} ({}).",
                source.provider.as_str(),
                source.path.display(),
                format_bytes(stats.bytes)
            ),
        )?;
        Ok(())
    }

    fn catalog_plugin(&mut self, source: &HistorySourcePluginSource) -> Result<()> {
        self.progress_mut()?.message(
            "cataloging",
            format!(
                "Cataloging provider-owned history source plugin path for {}.",
                source.label()
            ),
        )?;
        Ok(())
    }
}

impl IngestRefreshPort for CliIngestHost<'_> {
    fn refresh(
        &mut self,
        data_root: &Path,
        admission: Option<&ExplicitSourceCatalogAuthority>,
        no_daemon: bool,
    ) -> Result<IngestPublication> {
        let request = admission.map_or(ImportCoreRefreshRequest::Automatic, |authority| {
            ImportCoreRefreshRequest::ExplicitCatalog(authority)
        });
        let config = self.config;
        let refresh = wait_for_import_core_refresh(
            data_root,
            config,
            no_daemon,
            request,
            self.progress_mut()?,
        )?;
        let pinned_generation = refresh.pin.generation_id().to_owned();
        let policy_schema_hash = admission.is_none().then(|| {
            refresh
                .pin
                .into_index()
                .manifest()
                .policy_schema_hash
                .clone()
        });
        Ok(IngestPublication {
            request_id: refresh.request_id,
            request_previous_generation: refresh.request_previous_generation,
            request_generation_changed: refresh.request_generation_changed,
            scanned_routes: refresh.scanned_routes,
            pinned_generation,
            policy_schema_hash,
            receipt: refresh.receipt,
        })
    }
}

impl<'a> CliIngestHost<'a> {
    fn progress_mut(&mut self) -> Result<&mut ProgressReporter<'a>> {
        self.progress
            .as_mut()
            .context("ingest refresh requested before progress initialization")
    }
}

fn record_application_facts(
    outcome: &ctx_history_ingest_application::IngestReport,
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

#[cfg(test)]
mod tests {
    #[test]
    fn adapter_contains_no_parser_or_per_record_ingest_authority() {
        let source = include_str!("application_adapter.rs");
        for forbidden in [
            ["CtxHistory", "JsonlRecord"].concat(),
            ["Buf", "Read"].concat(),
            ["read", "_line"].concat(),
            ["SourceBackedRefresh", "Executor"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "application adapter contains forbidden implementation `{forbidden}`"
            );
        }
    }
}
