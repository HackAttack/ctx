mod context;
mod discovery;
mod lingma;
mod probes;
mod reasons;
mod resolvers;
mod selectors;
mod specs;
mod types;
mod warp;

use std::path::Path;

/// The Cursor-specific result shape discovery needs without taking ownership of
/// Cursor's inventory implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorTranscriptProbeOutcome {
    Found,
    NotFound,
    BudgetExhausted,
    IoError,
}

/// The only Trae parser result discovery needs to order generic SQLite probe
/// outcomes. Parser details stay in capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraePayloadProbeOutcome {
    SupportedChat,
    Empty,
    Incompatible,
}

pub type CursorTranscriptProbe = for<'a> fn(&'a Path) -> CursorTranscriptProbeOutcome;
pub type TraePayloadProbe = for<'a, 'b> fn(&'a [u8], &'b str) -> TraePayloadProbeOutcome;

#[derive(Debug, Clone, Copy)]
pub struct CursorProbeFragment {
    probe: CursorTranscriptProbe,
}

impl CursorProbeFragment {
    pub const fn new(probe: CursorTranscriptProbe) -> Self {
        Self { probe }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TraeProbeFragment {
    chat_keys: [&'static str; 6],
    chat_rows_query: &'static str,
    sqlite_value_overhead_bytes: u64,
    classify_payload: TraePayloadProbe,
}

impl TraeProbeFragment {
    pub const fn new(
        chat_keys: [&'static str; 6],
        chat_rows_query: &'static str,
        sqlite_value_overhead_bytes: u64,
        classify_payload: TraePayloadProbe,
    ) -> Self {
        Self {
            chat_keys,
            chat_rows_query,
            sqlite_value_overhead_bytes,
            classify_payload,
        }
    }
}

/// Closed composition seam for the two provider implementations that cannot
/// move into generic discovery. This type intentionally has no default,
/// optional fragments, registration, or dynamic lookup.
#[derive(Debug, Clone, Copy)]
pub struct StaticProviderProbeCatalog {
    cursor: CursorProbeFragment,
    trae: TraeProbeFragment,
}

impl StaticProviderProbeCatalog {
    pub const fn new(cursor: CursorProbeFragment, trae: TraeProbeFragment) -> Self {
        Self { cursor, trae }
    }
}

pub use context::{
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, DISCOVERY_ENV_ALLOWLIST,
};
pub use ctx_history_source_io::OrdinaryFileObservation;
pub(crate) use ctx_history_source_io::{
    observe_ordinary_file, open_ordinary_file_without_following,
};
#[cfg(test)]
pub(crate) use ctx_history_source_sqlite::{
    fail_next_opened_snapshot_cleanup_for_test, SqliteSourceDirectoryAuthority,
};
pub(crate) use ctx_history_source_sqlite::{
    open_root_handle_sqlite_source_snapshot_with_limits, retain_sqlite_source_directory_authority,
    SqliteSourceAccessError, SqliteSourceReadSnapshot, SqliteSourceSnapshotLimits,
};
pub use discovery::{
    discover_provider_sources, discover_provider_sources_for_provider,
    discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_context_and_work_budget,
    discover_provider_sources_with_projects, provider_source_for_path,
    provider_source_for_path_with_data_root, validate_provider_source_roots_outside_data_root,
    ProviderSourceRootBoundaryError,
};
pub use lingma::{
    discover_lingma_inventory_with_authority, resolve_lingma_discovery_authority,
    DiscoveredLingmaDatabase, LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
};

pub use resolvers::PathPresence;
pub use resolvers::{
    path_presence, CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError,
};
pub use specs::{provider_source_spec, provider_source_specs};
pub use types::{
    provider_source_status_reason, DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport,
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
pub use warp::{
    discover_warp_sources_with_authority, resolve_warp_discovery_authority, DiscoveredWarpSource,
    WarpDiscoveryUnavailable, WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel,
    WarpTerminalSurface,
};

#[cfg(test)]
pub(crate) const TRAE_CHAT_KEYS: [&str; 6] = [
    "memento/icube-ai-agent-storage",
    "icube-ai-agent-storage-input-history",
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];
#[cfg(test)]
pub(crate) const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;
#[cfg(test)]
pub(crate) const TEST_PROVIDER_PROBES: StaticProviderProbeCatalog = StaticProviderProbeCatalog::new(
    CursorProbeFragment::new(test_cursor_transcript_probe),
    TraeProbeFragment::new(
        TRAE_CHAT_KEYS,
        "select [key], count(*), typeof(value), coalesce(octet_length(value), 0), \
            case when count(*) = 1 \
                       and typeof(value) = 'text' \
                       and octet_length(value) + octet_length([key]) + ?7 <= ?8 \
                 then cast(value as text) end \
     from ItemTable \
     where [key] in (?1, ?2, ?3, ?4, ?5, ?6) \
     group by [key]",
        TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
        test_trae_payload_probe,
    ),
);

#[cfg(test)]
fn test_cursor_transcript_probe(path: &Path) -> CursorTranscriptProbeOutcome {
    const MAX_DIRECTORY_ENTRIES: usize = 1_024;
    const MAX_TRAVERSAL_ENTRIES: usize = 4_096;

    fn is_valid_transcript(projects: &Path, candidate: &Path) -> bool {
        let Ok(relative) = candidate.strip_prefix(projects) else {
            return false;
        };
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 4
            || components[1].as_os_str() != "agent-transcripts"
            || candidate
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("jsonl")
        {
            return false;
        }
        let Some(session) = components[2].as_os_str().to_str() else {
            return false;
        };
        !session.trim().is_empty()
            && candidate.file_stem().and_then(|name| name.to_str()) == Some(session)
    }

    fn selected_projects_root(path: &Path) -> std::path::PathBuf {
        if path.file_name().and_then(|name| name.to_str()) == Some(".cursor") {
            return path.join("projects");
        }
        path.ancestors()
            .find(|candidate| {
                candidate.file_name().and_then(|name| name.to_str()) == Some("projects")
            })
            .unwrap_or(path)
            .to_path_buf()
    }

    fn scan(
        path: &Path,
        projects: &Path,
        entries: &mut usize,
    ) -> Result<bool, CursorTranscriptProbeOutcome> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CursorTranscriptProbeOutcome::NotFound
            } else {
                CursorTranscriptProbeOutcome::IoError
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CursorTranscriptProbeOutcome::IoError);
        }
        if metadata.is_file() {
            return Ok(is_valid_transcript(projects, path));
        }
        if !metadata.is_dir() {
            return Ok(false);
        }
        let entries_in_directory = std::fs::read_dir(path)
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?;
        if entries_in_directory.len() > MAX_DIRECTORY_ENTRIES {
            return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
        }
        for entry in entries_in_directory {
            *entries = entries.saturating_add(1);
            if *entries > MAX_TRAVERSAL_ENTRIES {
                return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
            }
            if scan(&entry.path(), projects, entries)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    let projects = selected_projects_root(path);
    let mut entries = 0;
    match scan(&projects, &projects, &mut entries) {
        Ok(true) => CursorTranscriptProbeOutcome::Found,
        Ok(false) => CursorTranscriptProbeOutcome::NotFound,
        Err(outcome) => outcome,
    }
}

#[cfg(test)]
fn test_trae_payload_probe(payload: &[u8], _key: &str) -> TraePayloadProbeOutcome {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return TraePayloadProbeOutcome::Incompatible;
    };
    let Some(sessions) = value.get("list").and_then(serde_json::Value::as_array) else {
        return TraePayloadProbeOutcome::Incompatible;
    };
    if sessions.iter().any(|session| {
        session
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| !messages.is_empty())
    }) {
        TraePayloadProbeOutcome::SupportedChat
    } else {
        TraePayloadProbeOutcome::Empty
    }
}

#[cfg(test)]
mod tests;
