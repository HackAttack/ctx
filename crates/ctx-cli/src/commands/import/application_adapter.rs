use std::path::Path;

use anyhow::Result;
use ctx_history_capture::{source_backed_source_failure_identity, ProviderSource};
use ctx_history_core::CaptureProvider;
use ctx_history_ingest_application::{HistorySourcePluginSource, IngestPublication};
use ctx_history_platform::platform_security::establish_private_data_root;
use ctx_history_refresh::{ExplicitSourceCatalogAuthority, ExplicitSourceCatalogUpsert};

use crate::{
    history_source_plugins::prepare_source_backed_history_source, progress::ProgressReporter,
};

use super::{
    core_refresh::{wait_for_import_core_refresh, ImportCoreRefreshRequest},
    explicit_source_catalog::{
        explicit_source_for_admission, relocate_explicit_source, relocation_authority_for_import,
        upsert_explicit_source,
    },
};

/// Final-host implementation of the static import application port. This is
/// deliberately limited to concrete filesystem/catalog/daemon side effects.
pub(super) struct CliImportHost<'a> {
    config: &'a crate::config::AppConfig,
}

impl<'a> CliImportHost<'a> {
    pub(super) const fn new(config: &'a crate::config::AppConfig) -> Self {
        Self { config }
    }
}

impl ctx_history_cli::ImportApplicationPort for CliImportHost<'_> {
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

    fn refresh(
        &mut self,
        data_root: &Path,
        _config: ctx_history_cli::HistoryCliConfig,
        admission: Option<&ExplicitSourceCatalogAuthority>,
        no_daemon: bool,
        progress: &mut ProgressReporter<'_>,
    ) -> Result<IngestPublication> {
        let request = admission.map_or(ImportCoreRefreshRequest::Automatic, |authority| {
            ImportCoreRefreshRequest::ExplicitCatalog(authority)
        });
        let refresh =
            wait_for_import_core_refresh(data_root, self.config, no_daemon, request, progress)?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn final_host_contains_only_concrete_side_effect_adapters() {
        let source = include_str!("application_adapter.rs");
        for forbidden in [
            ["Ingest", "Request"].concat(),
            ["run", "_ingest"].concat(),
            ["Source", "Discovery", "Port"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "final import host contains application authority `{forbidden}`"
            );
        }
    }
}
