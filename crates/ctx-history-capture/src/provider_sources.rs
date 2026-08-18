//! Capture compatibility facade for provider-neutral source discovery.

use std::path::{Path, PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_provider_claude_cursor::{discover_cursor_transcripts, CursorDiscoveryIssueKind};
use ctx_history_source_discovery::{
    CursorProbeFragment, CursorTranscriptProbeOutcome, StaticProviderProbeCatalog,
};

pub use ctx_history_source_discovery::{
    path_presence, CrushDiscoveredProjectInventory, CrushProjectInventorySelectorError,
};
pub use ctx_history_source_discovery::{
    validate_provider_source_roots_outside_data_root, DiscoveredLingmaDatabase,
    DiscoveredWarpSource, DiscoveryContext, DiscoveryIssue, DiscoveryIssueKind, DiscoveryPlatform,
    DiscoveryPlatformDirs, DiscoveryReport, LingmaDatabaseCatalogLineage,
    LingmaDiscoveredInventory, LingmaDiscoveryUnavailable, LingmaVscodeClient, LingmaVscodeProfile,
    PathPresence, ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport,
    ProviderSource, ProviderSourceKind, ProviderSourceRootBoundaryError, ProviderSourceSpec,
    ProviderSourceStatus, ProviderSourceStatusReason, WarpDiscoveryUnavailable,
    WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel, WarpTerminalSurface,
    DISCOVERY_ENV_ALLOWLIST,
};
pub use ctx_history_source_io::OrdinaryFileObservation;
ctx_history_source_io::define_mapped_ordinary_io_compat!(crate::CaptureError);

pub(crate) static BUILTIN_PROVIDER_PROBES: StaticProviderProbeCatalog =
    StaticProviderProbeCatalog::new(CursorProbeFragment::new(probe_cursor_transcripts));

fn probe_cursor_transcripts(path: &Path) -> CursorTranscriptProbeOutcome {
    let input = if path.is_dir() && path.join("projects").is_dir() {
        path.join("projects")
    } else {
        path.to_path_buf()
    };
    let inventory = discover_cursor_transcripts(&input);
    if !inventory.completed() {
        if inventory.has_issue_kind(CursorDiscoveryIssueKind::LimitExceeded) {
            return CursorTranscriptProbeOutcome::BudgetExhausted;
        }
        if inventory.has_issue_kind(CursorDiscoveryIssueKind::Io)
            || inventory.has_issue_kind(CursorDiscoveryIssueKind::Symlink)
        {
            return CursorTranscriptProbeOutcome::IoError;
        }
        return CursorTranscriptProbeOutcome::NotFound;
    }
    if !inventory.has_transcripts() {
        CursorTranscriptProbeOutcome::NotFound
    } else {
        CursorTranscriptProbeOutcome::Found
    }
}

pub fn discover_lingma_inventory_with_authority(
    context: &DiscoveryContext,
) -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable> {
    ctx_history_source_discovery::discover_lingma_inventory_with_authority(
        &BUILTIN_PROVIDER_PROBES,
        context,
    )
}

pub fn resolve_lingma_discovery_authority(
    context: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> std::result::Result<DiscoveredLingmaDatabase, LingmaDiscoveryUnavailable> {
    ctx_history_source_discovery::resolve_lingma_discovery_authority(
        &BUILTIN_PROVIDER_PROBES,
        context,
        selected_source,
    )
}

pub fn discover_warp_sources_with_authority(
    context: &DiscoveryContext,
) -> std::result::Result<Vec<DiscoveredWarpSource>, WarpDiscoveryUnavailable> {
    ctx_history_source_discovery::discover_warp_sources_with_authority(
        &BUILTIN_PROVIDER_PROBES,
        context,
    )
}

pub fn resolve_warp_discovery_authority(
    context: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> std::result::Result<DiscoveredWarpSource, WarpDiscoveryUnavailable> {
    ctx_history_source_discovery::resolve_warp_discovery_authority(
        &BUILTIN_PROVIDER_PROBES,
        context,
        selected_source,
    )
}

#[derive(Debug, Clone)]
pub struct LingmaInventorySelector(ctx_history_source_discovery::LingmaInventorySelector);

impl LingmaInventorySelector {
    pub fn new(context: DiscoveryContext) -> Self {
        Self(ctx_history_source_discovery::LingmaInventorySelector::new(
            context,
            BUILTIN_PROVIDER_PROBES,
        ))
    }

    pub fn observe(
        &self,
    ) -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable> {
        self.0.observe()
    }
}

#[derive(Debug, Clone)]
pub struct CrushProjectInventorySelector(
    ctx_history_source_discovery::CrushProjectInventorySelector,
);

impl CrushProjectInventorySelector {
    pub fn new(context: DiscoveryContext) -> Self {
        Self(
            ctx_history_source_discovery::CrushProjectInventorySelector::new(
                context,
                BUILTIN_PROVIDER_PROBES,
            ),
        )
    }

    pub fn observe(
        &self,
        spec: &ProviderSourceSpec,
    ) -> std::result::Result<CrushDiscoveredProjectInventory, CrushProjectInventorySelectorError>
    {
        self.0.observe(spec)
    }
}

pub fn discover_provider_sources(home: &Path) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources(&BUILTIN_PROVIDER_PROBES, home)
}

pub fn discover_provider_sources_report(home: &Path) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_report(&BUILTIN_PROVIDER_PROBES, home)
}

pub fn discover_provider_sources_with_context(context: &DiscoveryContext) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_with_context(
        &BUILTIN_PROVIDER_PROBES,
        context,
    )
}

pub fn discover_provider_sources_with_context_and_work_budget(
    context: &DiscoveryContext,
    worker_limit: usize,
) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_with_context_and_work_budget(
        &BUILTIN_PROVIDER_PROBES,
        context,
        worker_limit,
    )
}

pub fn discover_provider_sources_with_projects(
    home: &Path,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources_with_projects(
        &BUILTIN_PROVIDER_PROBES,
        home,
        project_locators,
    )
}

pub fn discover_provider_sources_for_provider(
    home: &Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources_for_provider(
        &BUILTIN_PROVIDER_PROBES,
        home,
        provider,
    )
}

pub fn discover_provider_sources_for_provider_report(
    home: &Path,
    provider: CaptureProvider,
) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_for_provider_report(
        &BUILTIN_PROVIDER_PROBES,
        home,
        provider,
    )
}

pub fn discover_provider_sources_for_provider_with_context(
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &BUILTIN_PROVIDER_PROBES,
        context,
        provider,
    )
}

pub fn discover_provider_sources_for_provider_with_projects(
    home: &Path,
    provider: CaptureProvider,
    project_locators: &[PathBuf],
) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_projects(
        &BUILTIN_PROVIDER_PROBES,
        home,
        provider,
        project_locators,
    )
}

pub fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    ctx_history_source_discovery::provider_source_for_path(&BUILTIN_PROVIDER_PROBES, provider, path)
}

pub fn provider_source_spec(provider: CaptureProvider) -> Option<&'static ProviderSourceSpec> {
    ctx_history_source_discovery::provider_source_spec(provider)
}

pub fn provider_source_specs() -> &'static [ProviderSourceSpec] {
    ctx_history_source_discovery::provider_source_specs()
}

pub fn provider_source_status_reason(
    source: &ProviderSource,
) -> Option<ProviderSourceStatusReason> {
    ctx_history_source_discovery::provider_source_status_reason(source)
}

#[cfg(test)]
mod extraction_regression_tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary source-discovery fixture")
    }

    fn context(root: &Path) -> DiscoveryContext {
        DiscoveryContext::new(
            root.join("home"),
            root.join("cwd"),
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs {
                config: Some(root.join("config")),
                ..DiscoveryPlatformDirs::default()
            },
        )
    }

    #[test]
    fn capture_catalog_cursor_uses_importer_specific_default_layout() {
        let temp = tempdir();
        let transcript = temp
            .path()
            .join(".cursor/projects/project/agent-transcripts/session/session.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();
        assert_eq!(
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Cursor)[0].status,
            ProviderSourceStatus::Available
        );

        let empty = tempdir();
        let lookalike = empty
            .path()
            .join(".cursor/projects/project/agent-transcripts/session/wrong.jsonl");
        std::fs::create_dir_all(lookalike.parent().unwrap()).unwrap();
        std::fs::write(lookalike, b"{}\n").unwrap();
        assert_eq!(
            discover_provider_sources_for_provider(empty.path(), CaptureProvider::Cursor)[0].status,
            ProviderSourceStatus::Empty
        );
    }

    #[test]
    fn capture_facade_matches_direct_catalog_and_serial_work_budget_order() {
        let temp = tempdir();
        let context = context(temp.path());
        let facade = discover_provider_sources_with_context(&context);
        let direct = ctx_history_source_discovery::discover_provider_sources_with_context(
            &BUILTIN_PROVIDER_PROBES,
            &context,
        );
        let bounded = discover_provider_sources_with_context_and_work_budget(&context, 1);

        assert_eq!(facade, direct);
        assert_eq!(facade, bounded);
    }
}
