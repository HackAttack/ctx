use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CoreRecord, CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SessionRelationshipKind,
    SourceAnchor, SourceFrontier, SourceKey, SourceObservation, StableEntityId, TypedKey,
};
use ctx_history_index::{BaseEventIdentityLookup, IndexError};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    discover_codex_catalog_sources,
    reader::{
        opened_file_prefix_sha256, reopen_codex_source_capability,
        revalidate_codex_catalog_source_capability, CodexParseDisposition, CodexScanCounters,
    },
    rows::{
        CodexProviderEventIdentityKindV0, CodexProviderEventIdentityV0, CodexSourceBackedRowV0,
        MAX_CODEX_DURABLE_METADATA_BYTES,
    },
    source::{CodexCatalogSource, CodexFileObservation, CodexSourceIdentity},
    CodexAppendProof, CodexCheckpointGeneration, CodexNativeCheckpoint, CodexNativeOwnedPage,
    CodexNativeScanner, CodexSessionRow, CodexSourceScan,
};
use crate::repository_attribution::{apply_annotation, merge_repository_annotation};
use crate::{
    common::io::{
        open_provider_source_file, OpenedProviderSourcePath, ProviderSourceRoot,
        PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
        PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
    },
    provider::codex::{
        catalog::catalog_codex_explicit_session_opened, nativepath::opened_codex_file_observation,
    },
    CaptureError, CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_SOURCE_ANCHOR_NAMESPACE: &str = "codex.session";
const CODEX_NATIVE_SESSION_NAMESPACE: &str = "codex.session";
const CODEX_LOGICAL_SESSION_KIND: &str = "codex-session";
const CODEX_LOGICAL_EVENT_KIND: &str = "codex-event";
const CODEX_SOURCE_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const CODEX_SOURCE_REVISION_KIND: &str = "codex-ordinary-file-observation-v1";
const CODEX_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v14";
const CODEX_PARSER_REVISION: &str = "codex-nativepath-core-record-v27-bounded-exact-origin";

#[derive(Debug, Error)]
pub enum CodexSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Codex catalog discovery rejected {rejected} sources and failed {failed} sources")]
    IncompleteCatalog { rejected: usize, failed: usize },
    #[error("Codex catalog source {path:?} has no native session ID")]
    MissingNativeSessionId { path: PathBuf },
    #[error("Codex source {0:?} is not a cold source or exact append")]
    UnsupportedLifecycle(String),
    #[error("Codex source certificate has no NativePath checkpoint frontier")]
    MissingCheckpoint,
    #[error("Codex source certificate has an unsupported checkpoint kind or payload")]
    InvalidCheckpoint,
    #[error("Codex scanner emitted a row without lexical body text")]
    MissingLexicalBody,
    #[error("Codex scanner emitted a row without its native session owner")]
    MissingPageOwner,
    #[error("Codex scanner owner {actual:?} does not match catalog owner {expected:?}")]
    OwnerMismatch { expected: String, actual: String },
    #[error("Codex scan counters do not reconcile with streamed Core records")]
    ScanCountMismatch,
    #[error("Codex source count overflow")]
    CountOverflow,
    #[error("Codex generation participant count overflow")]
    GenerationParticipantCountOverflow,
    #[error("Codex generation coordinator is unavailable")]
    GenerationCoordinatorUnavailable,
    #[error("Codex source-backed scanner emitted a legacy Core publication row")]
    UnexpectedLegacyRow,
    #[error("explicit Codex session source changed its native session identity")]
    ExplicitSourceIdentityChanged,
}

pub type CodexSourceBackedResultV0<T> = Result<T, CodexSourceBackedErrorV0>;

#[cfg(any(test, ctx_codex_causal_qualification))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexSourceBackedCountersV0 {
    pub catalog_sources: u64,
    pub catalog_source_bytes: u64,
    pub inventory_walks: u64,
    pub inventory_source_observations: u64,
    pub catalog_source_metadata_opens: u64,
    pub catalog_source_metadata_read_upper_bound_bytes: u64,
    pub catalog_session_meta_parses: u64,
    pub cold_sources: u64,
    pub appended_sources: u64,
    pub replaced_sources: u64,
    pub replayed_sources: u64,
    pub deleted_sources: u64,
    pub writer_exact_replay_sources: u64,
    pub writer_mutated_sources: u64,
    pub scanner_workers: u64,
    pub scanner_source_opens: u64,
    pub scanner_sources_started: u64,
    pub scanner_sources_completed: u64,
    pub peak_active_scanners: u64,
    pub repository_full_git_certification_probes: u64,
    pub staged_documents: u64,
    pub complete_records_scanned: u64,
    pub retained_records_scanned: u64,
    pub rejected_records_scanned: u64,
    pub ignored_records_scanned: u64,
    pub scanner_bytes_read: u64,
    pub checkpoint_validation_bytes: u64,
    pub mcp_terminal_authority_bytes_read: u64,
    pub peak_mcp_terminal_authority_entries: usize,
    pub peak_mcp_terminal_authority_bytes: usize,
    pub prefiltered_records: u64,
    pub structural_json_parses: u64,
    pub typed_json_parses: u64,
    pub emitted_pages: u64,
    pub scanner_legacy_body_json_serializations: u64,
    pub scanner_legacy_row_json_serializations: u64,
    pub scanner_legacy_json_serialized_bytes: u64,
    pub scanner_legacy_normalized_payload_hashes: u64,
    pub scanner_legacy_file_touch_rows: u64,
    pub scanner_legacy_duplicate_preview_allocations: u64,
    pub scanner_legacy_page_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_row_json_serializations: u64,
}

#[cfg(any(test, ctx_codex_causal_qualification))]
impl CodexSourceBackedCountersV0 {
    fn add_assign(&mut self, other: Self) {
        macro_rules! add {
            ($($field:ident),+ $(,)?) => {
                $(self.$field = self.$field.saturating_add(other.$field);)+
            };
        }
        add!(
            catalog_sources,
            catalog_source_bytes,
            inventory_walks,
            inventory_source_observations,
            catalog_source_metadata_opens,
            catalog_source_metadata_read_upper_bound_bytes,
            catalog_session_meta_parses,
            cold_sources,
            appended_sources,
            replaced_sources,
            replayed_sources,
            deleted_sources,
            writer_exact_replay_sources,
            writer_mutated_sources,
            scanner_sources_started,
            scanner_sources_completed,
            scanner_source_opens,
            repository_full_git_certification_probes,
            staged_documents,
            complete_records_scanned,
            retained_records_scanned,
            rejected_records_scanned,
            ignored_records_scanned,
            scanner_bytes_read,
            checkpoint_validation_bytes,
            mcp_terminal_authority_bytes_read,
            prefiltered_records,
            structural_json_parses,
            typed_json_parses,
            emitted_pages,
            scanner_legacy_body_json_serializations,
            scanner_legacy_row_json_serializations,
            scanner_legacy_json_serialized_bytes,
            scanner_legacy_normalized_payload_hashes,
            scanner_legacy_file_touch_rows,
            scanner_legacy_duplicate_preview_allocations,
            scanner_legacy_page_owner_json_serializations,
            scanner_legacy_page_identity_owner_json_serializations,
            scanner_legacy_page_identity_row_json_serializations,
        );
        self.scanner_workers = self.scanner_workers.max(other.scanner_workers);
        self.peak_active_scanners = self.peak_active_scanners.max(other.peak_active_scanners);
        self.peak_mcp_terminal_authority_entries = self
            .peak_mcp_terminal_authority_entries
            .max(other.peak_mcp_terminal_authority_entries);
        self.peak_mcp_terminal_authority_bytes = self
            .peak_mcp_terminal_authority_bytes
            .max(other.peak_mcp_terminal_authority_bytes);
    }

    pub(crate) fn add_catalog_work(&mut self, work: CodexCatalogWorkV0) {
        self.inventory_walks = self.inventory_walks.saturating_add(work.inventory_walks);
        self.inventory_source_observations = self
            .inventory_source_observations
            .saturating_add(work.source_observations);
        self.catalog_source_metadata_opens = self
            .catalog_source_metadata_opens
            .saturating_add(work.source_metadata_opens);
        self.catalog_source_metadata_read_upper_bound_bytes = self
            .catalog_source_metadata_read_upper_bound_bytes
            .saturating_add(work.source_metadata_read_upper_bound_bytes);
        self.catalog_session_meta_parses = self
            .catalog_session_meta_parses
            .saturating_add(work.session_meta_parses);
    }

    fn add_scan(&mut self, scan: CodexScanCounters) {
        self.complete_records_scanned = self
            .complete_records_scanned
            .saturating_add(scan.complete_records);
        self.retained_records_scanned = self
            .retained_records_scanned
            .saturating_add(scan.retained_records);
        self.rejected_records_scanned = self
            .rejected_records_scanned
            .saturating_add(scan.rejected_complete_records);
        let classified = scan
            .retained_records
            .saturating_add(scan.rejected_complete_records);
        self.ignored_records_scanned = self
            .ignored_records_scanned
            .saturating_add(scan.complete_records.saturating_sub(classified));
        self.scanner_bytes_read = self.scanner_bytes_read.saturating_add(scan.bytes_read);
        self.checkpoint_validation_bytes = self
            .checkpoint_validation_bytes
            .saturating_add(scan.checkpoint_validation_bytes);
        self.mcp_terminal_authority_bytes_read = self
            .mcp_terminal_authority_bytes_read
            .saturating_add(scan.mcp_terminal_authority_bytes_read);
        self.peak_mcp_terminal_authority_entries = self
            .peak_mcp_terminal_authority_entries
            .max(scan.peak_mcp_terminal_authority_entries);
        self.peak_mcp_terminal_authority_bytes = self
            .peak_mcp_terminal_authority_bytes
            .max(scan.peak_mcp_terminal_authority_bytes);
        self.prefiltered_records = self
            .prefiltered_records
            .saturating_add(scan.prefiltered_records);
        self.structural_json_parses = self
            .structural_json_parses
            .saturating_add(scan.structural_json_parses);
        self.typed_json_parses = self
            .typed_json_parses
            .saturating_add(scan.typed_json_parses);
        self.emitted_pages = self.emitted_pages.saturating_add(scan.emitted_pages);
        self.scanner_legacy_body_json_serializations = self
            .scanner_legacy_body_json_serializations
            .saturating_add(scan.legacy_body_json_serializations);
        self.scanner_legacy_row_json_serializations = self
            .scanner_legacy_row_json_serializations
            .saturating_add(scan.legacy_row_json_serializations);
        self.scanner_legacy_json_serialized_bytes = self
            .scanner_legacy_json_serialized_bytes
            .saturating_add(scan.legacy_json_serialized_bytes);
        self.scanner_legacy_normalized_payload_hashes = self
            .scanner_legacy_normalized_payload_hashes
            .saturating_add(scan.retained_hashes_created);
        self.scanner_legacy_file_touch_rows = self
            .scanner_legacy_file_touch_rows
            .saturating_add(scan.legacy_file_touch_rows_created);
        self.scanner_legacy_page_owner_json_serializations = self
            .scanner_legacy_page_owner_json_serializations
            .saturating_add(scan.legacy_page_owner_json_serializations);
        self.scanner_legacy_page_identity_owner_json_serializations = self
            .scanner_legacy_page_identity_owner_json_serializations
            .saturating_add(scan.legacy_page_identity_owner_json_serializations);
        self.scanner_legacy_page_identity_row_json_serializations = self
            .scanner_legacy_page_identity_row_json_serializations
            .saturating_add(scan.legacy_page_identity_row_json_serializations);
    }
}

mod catalog;
#[cfg(any(test, ctx_codex_causal_qualification))]
mod causal;
mod generation;
mod identity;
mod ingestion;
mod jsonl_family;
mod origin;

use origin::CodexOutcomeOriginV0;

use catalog::discover_codex_session_tree_inventory_v0;
#[cfg(test)]
pub(crate) use catalog::install_after_codex_metadata_inventory_hook;
#[cfg(any(test, ctx_codex_causal_qualification))]
use catalog::CodexCatalogWorkV0;
pub(crate) use catalog::{
    observe_codex_explicit_session_source_backed_v0, CodexExplicitSessionSourceBackedInputV0,
    CodexSessionTreeInventoryV0,
};
#[cfg(test)]
pub(crate) use causal::{install_after_codex_causal_stage_hook_v1, CodexCausalSourceObservationV1};
pub(crate) use generation::{
    CodexGenerationCarriedRouteV0, CodexGenerationNormalizationCoordinatorV0,
    CodexGenerationRouteV0,
};
use identity::{
    certify_scan, codex_core_record, codex_session_identity, codex_source_key, decode_append_proof,
    validate_owner, CodexEventIdentityStateV0,
};
use ingestion::{
    scan_codex_jsonl_family_leaf_v0, CodexJsonlFamilyLeafContextV0, CodexJsonlFamilyPublicationV0,
};
pub(crate) use jsonl_family::{
    codex_session_root_rank, CodexExplicitSessionJsonlFamilyAdapterV0,
    CodexSessionTreeJsonlFamilyAdapterV0,
};

#[cfg(test)]
mod counter_tests {
    use super::*;

    #[test]
    fn aggregation_sums_mcp_reads_and_takes_mcp_peaks() {
        let mut total = CodexSourceBackedCountersV0 {
            mcp_terminal_authority_bytes_read: 11,
            peak_mcp_terminal_authority_entries: 7,
            peak_mcp_terminal_authority_bytes: 70,
            ..CodexSourceBackedCountersV0::default()
        };
        total.add_assign(CodexSourceBackedCountersV0 {
            mcp_terminal_authority_bytes_read: 13,
            peak_mcp_terminal_authority_entries: 5,
            peak_mcp_terminal_authority_bytes: 90,
            ..CodexSourceBackedCountersV0::default()
        });
        assert_eq!(total.mcp_terminal_authority_bytes_read, 24);
        assert_eq!(total.peak_mcp_terminal_authority_entries, 7);
        assert_eq!(total.peak_mcp_terminal_authority_bytes, 90);
    }
}
