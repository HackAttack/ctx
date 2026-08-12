//! Shared coordination for provider-owned source-backed projections.
//!
//! Provider adapters remain responsible for discovery, parsing, projection,
//! and source certification. This module owns the one production
//! publication boundary: all registered adapters stage into one neutral
//! lifecycle and no adapter can publish a generation by itself.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
    CaptureSourceAggregateRef, ImmutableCaptureSnapshot,
};
use ctx_history_core::SourceAnchor;
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, ScannedSourceCounts, SourceKey,
    TypedKey,
};
#[cfg(test)]
use ctx_history_core::{CertifiedSourceAppend, CertifiedSourceDeletion};
use ctx_history_index::{IndexError, PublicationStage, WriterOptions};
use sha2::{Digest, Sha256};

use super::codex::nativepath::CodexGenerationNormalizationCoordinatorV0;
pub use super::providers::crush::native_path::source_backed::{
    CrushProjectDatabaseV0, CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0,
};
use super::providers::{
    astrbot::native_path::source_backed::{
        AstrBotSourceBackedInventoryV0, AstrBotSourceBackedSourceV0,
        PARSER_REVISION as ASTRBOT_SOURCE_BACKED_PARSER_REVISION, scan_astrbot_snapshot_v0,
    },
    continue_cli::native_path::{ContinueSourceBackedOutcome, ContinueSourceBackedReader},
    crush::native_path::source_backed::{
        CRUSH_PARSER_REVISION, CrushSourceBackedErrorV0, CrushSourceBackedResultV0,
        bind_inventory as bind_crush_inventory, finish_opened_source as finish_crush_source,
        scan_source as scan_crush_source,
    },
    deepagents::native_path::source_backed::DeepAgentsDatabaseSelectionV0,
    forgecode::nativepath::source_backed::ForgeCodeSourceSelectionV0,
    goose::{GooseSourceBackedAdapterV0, GooseSourceBackedSelectionV0, GooseSourceRouteV0},
    hermes::source_backed::{HermesSourceCandidate, hermes_source_backed_explicit},
    lingma::native_path::{
        LINGMA_SOURCE_BACKED_PARSER_REVISION, LingmaDatabaseSourceV0, LingmaSourceBackedErrorV0,
        LingmaSourceBackedResultV0, LingmaSourceInventoryV0,
        reject_duplicate_paths as reject_duplicate_lingma_paths, scan_lingma_snapshot_v0,
    },
    mistral_vibe::native_path::source_backed::scan_mistral_vibe_source_backed,
    mux::mux_jsonl_adapter,
    nanoclaw::native_path::source_backed::NanoClawDocumentTreeAdapter,
    openhands::nativepath::OpenHandsEventFileAdapterV2,
    rovodev::native_path::RovoDevDocumentTreeAdapter,
    shelley::native_path::source_backed::{
        SHELLEY_SOURCE_PARSER_REVISION, ShelleySourceBackedAdapter,
        discover_shelley_source_backed_exact_cwd,
    },
    task_json::cline_nativepath::{
        cline_task_json_source_backed_adapter, roo_task_json_source_backed_adapter,
    },
    warp::{WarpSourceSelectionV0, project_warp_source_backed_v0},
    zed::native_path::source_backed::{
        ZED_PARSER_REVISION, ZedSourceBackedSinkV0, acquire_snapshot as acquire_zed_snapshot,
        decode_sha256_hex as decode_zed_digest, scan_zed_native_snapshot,
        snapshot_revision_digest as zed_snapshot_revision_digest,
        source_observation as zed_source_observation, zed_source_key,
    },
};
use crate::provider_sources::{
    CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError, LingmaDiscoveryUnavailable, LingmaInventorySelector,
    PathPresence, WarpDiscoveryUnavailable, path_presence, resolve_warp_discovery_authority,
};
use crate::{
    CaptureError, DiscoveryContext, DiscoveryIssue, DiscoveryPlatform, DiscoveryReport,
    ProviderAdapterContext, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus, discover_provider_sources_with_context,
    provider_source_spec, validate_provider_source_roots_outside_data_root,
};

mod discovery;
mod driver;
pub(crate) mod family;
mod inventory;
mod publication;
mod registration;
mod runtime_adapter;
mod watch;

pub(crate) use ctx_history_capture_runtime::BaseEventLookup;
pub use ctx_history_capture_runtime::{
    CaptureLifecycleOpenOutcome, CaptureRevalidationTarget, PresentCaptureRoute,
};
pub use discovery::*;
pub use driver::*;
#[cfg(test)]
pub(crate) use family::jsonl::FallbackEventIdentityMode;
pub(crate) use family::jsonl::FallbackEventIdentityState;
pub use inventory::*;
pub use publication::*;
pub use registration::*;
pub(crate) use runtime_adapter::*;
pub use runtime_adapter::{
    BorrowedIndexManifestView, CommittedIndexManifestView, IndexCaptureCommitReceipt,
    IndexCaptureLifecycle, IndexCaptureVerifiedPin, IndexManifestView, IndexVerifiedCapture,
};
pub use watch::*;

pub(crate) fn source_backed_base_sources(
    sink: &SourceBackedGenerationSink<'_>,
    owns: impl Fn(&SourceKey) -> bool,
) -> Vec<CertifiedSource> {
    sink.lifecycle
        .base_snapshot()
        .map(|manifest| {
            manifest
                .sources()
                .iter()
                .filter(|source| owns(source.observation().source()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
