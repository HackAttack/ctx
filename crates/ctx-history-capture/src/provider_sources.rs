//! Capture compatibility facade for provider-neutral source discovery.

use std::path::{Path, PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::{
    CursorProbeFragment, CursorTranscriptProbeOutcome, StaticProviderProbeCatalog,
    TraePayloadProbeOutcome, TraeProbeFragment,
};

use crate::provider::providers::trae::{
    trae_payload_admission, TraePayloadAdmission, TRAE_CHAT_KEYS, TRAE_CHAT_ROWS_QUERY,
    TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
};
use ctx_history_provider_claude_cursor::{discover_cursor_transcripts, CursorDiscoveryIssueKind};

pub(crate) use crate::provider::sqlite::{
    sqlite_retry_decision, SqliteLogicalSnapshot, SqliteRetryDecision,
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
#[cfg(test)]
pub(crate) use ctx_history_source_io::count_event_file_io;
pub(crate) use ctx_history_source_io::{
    EventFileCoordinates, EventFileGroup, EventFileInventory, EventFileInventoryError,
    EventFileLimits,
};
#[cfg(test)]
pub(crate) use ctx_history_source_sqlite::{
    fail_next_opened_snapshot_cleanup_for_test, force_next_pinned_wal_unavailable_for_test,
    SqliteSourceSnapshotCounters,
};
pub(crate) use ctx_history_source_sqlite::{
    open_root_handle_sqlite_source_snapshot, resource_exhaustion_io_error,
    retain_sqlite_source_directory_authority, rusqlite_busy_or_locked, rusqlite_resource_failure,
    SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
    SqliteSourceDirectoryAuthority, SqliteSourceErrorComposition, SqliteSourceEvidence,
    SqliteSourceProgressError, SqliteSourceReadSnapshot,
};

static BUILTIN_PROVIDER_PROBES: StaticProviderProbeCatalog = StaticProviderProbeCatalog::new(
    CursorProbeFragment::new(probe_cursor_transcripts),
    TraeProbeFragment::new(
        [
            TRAE_CHAT_KEYS[0],
            TRAE_CHAT_KEYS[1],
            TRAE_CHAT_KEYS[2],
            TRAE_CHAT_KEYS[3],
            TRAE_CHAT_KEYS[4],
            TRAE_CHAT_KEYS[5],
        ],
        TRAE_CHAT_ROWS_QUERY,
        TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
        classify_trae_payload_for_discovery,
    ),
);

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

fn classify_trae_payload_for_discovery(bytes: &[u8], chat_key: &str) -> TraePayloadProbeOutcome {
    match trae_payload_admission(bytes, chat_key) {
        Ok(TraePayloadAdmission::SupportedChat) => TraePayloadProbeOutcome::SupportedChat,
        Ok(TraePayloadAdmission::Empty) => TraePayloadProbeOutcome::Empty,
        Ok(TraePayloadAdmission::Unrecognized) | Err(_) => TraePayloadProbeOutcome::Incompatible,
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
    use rusqlite::{types::Value, Connection};

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

    fn trae_database(context: &DiscoveryContext) -> PathBuf {
        let path = context
            .platform_dirs()
            .config
            .as_ref()
            .unwrap()
            .join("Trae/ModularData/ai-agent/database.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("create table ItemTable ([key] text, value)", [])
            .unwrap();
        path
    }

    fn trae_status(context: &DiscoveryContext) -> ProviderSourceStatus {
        discover_provider_sources_for_provider_with_context(context, CaptureProvider::Trae)
            .sources
            .into_iter()
            .next()
            .unwrap()
            .status
    }

    #[test]
    fn capture_catalog_trae_supported_payload_wins_over_malformed_sibling() {
        let temp = tempdir();
        let context = context(temp.path());
        let database = trae_database(&context);
        let connection = Connection::open(database).unwrap();
        connection
            .execute(
                "insert into ItemTable ([key], value) values (?1, ?2), (?3, ?4)",
                rusqlite::params![
                    TRAE_CHAT_KEYS[0],
                    r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                    TRAE_CHAT_KEYS[1],
                    "invalid JSON",
                ],
            )
            .unwrap();

        assert_eq!(trae_status(&context), ProviderSourceStatus::Available);
    }

    #[test]
    fn capture_catalog_trae_rejects_invalid_non_text_and_unrecognized_payloads() {
        for value in [
            Value::Text("arbitrary nonempty garbage".to_owned()),
            Value::Blob(br#"{"list":[]}"#.to_vec()),
            Value::Text(r#"{"futureSessions":[]}"#.to_owned()),
        ] {
            let temp = tempdir();
            let context = context(temp.path());
            let database = trae_database(&context);
            Connection::open(database)
                .unwrap()
                .execute(
                    "insert into ItemTable ([key], value) values (?1, ?2)",
                    rusqlite::params![TRAE_CHAT_KEYS[0], value],
                )
                .unwrap();
            assert_eq!(trae_status(&context), ProviderSourceStatus::Unknown);
        }
    }

    #[test]
    fn capture_catalog_trae_status_matrix_preserves_missing_empty_valid_and_malformed() {
        let temp = tempdir();
        let missing = context(temp.path());
        assert_eq!(trae_status(&missing), ProviderSourceStatus::Missing);

        for (payload, expected) in [
            (r#"{"list":[]}"#, ProviderSourceStatus::Empty),
            (
                r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                ProviderSourceStatus::Available,
            ),
            ("invalid JSON", ProviderSourceStatus::Unknown),
        ] {
            let temp = tempdir();
            let context = context(temp.path());
            let database = trae_database(&context);
            Connection::open(database)
                .unwrap()
                .execute(
                    "insert into ItemTable ([key], value) values (?1, ?2)",
                    rusqlite::params![TRAE_CHAT_KEYS[0], payload],
                )
                .unwrap();
            assert_eq!(trae_status(&context), expected);
        }
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
