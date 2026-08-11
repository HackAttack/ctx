use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    SessionIdentityInput, SessionRelationshipKind, SourceAnchor, SourceKey, StableEntityId,
    TypedKey,
};
use ctx_history_index::{BaseEventIdentityLookup, IndexError};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(any(test, ctx_codex_causal_qualification))]
use super::reader::CodexScanCounters;
use super::{
    discover_codex_catalog_sources,
    reader::{
        opened_file_prefix_sha256, reopen_codex_source_capability,
        revalidate_codex_catalog_source_capability,
    },
    rows::{
        CodexCoreRecordDraft, CodexProviderEventIdentityKindV0, CodexProviderEventIdentityV0,
        MAX_CODEX_DURABLE_METADATA_BYTES,
    },
    source::{CodexCatalogSource, CodexFileObservation},
    CodexNativeScanner, CodexSessionRow,
};
use crate::repository_attribution::{apply_annotation, merge_repository_annotation};
use crate::{
    common::io::{
        open_provider_source_file, OpenedProviderSourcePath, ProviderSourceRoot,
        PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
        PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
    },
    provider::codex::{
        catalog::catalog_codex_explicit_session_opened, events::CodexInvocationOriginV0,
        nativepath::opened_codex_file_observation,
    },
    CaptureError, CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_SOURCE_ANCHOR_NAMESPACE: &str = "codex.session";
const CODEX_NATIVE_SESSION_NAMESPACE: &str = "codex.session";
const CODEX_LOGICAL_SESSION_KIND: &str = "codex-session";
const CODEX_LOGICAL_EVENT_KIND: &str = "codex-event";
const CODEX_SOURCE_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const CODEX_PARSER_REVISION: &str =
    "codex-nativepath-core-record-v31-repository-positive-exact-authority";

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

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
    #[error("Codex source certificate has an unsupported checkpoint kind or payload")]
    InvalidCheckpoint,
    #[error("Codex scanner emitted a row without lexical body text")]
    MissingLexicalBody,
    #[error("Codex source count overflow")]
    CountOverflow,
    #[error("Codex generation participant count overflow")]
    GenerationParticipantCountOverflow,
    #[error("Codex generation coordinator is unavailable")]
    GenerationCoordinatorUnavailable,
    #[error("explicit Codex session source changed its native session identity")]
    ExplicitSourceIdentityChanged,
}

pub type CodexSourceBackedResultV0<T> = Result<T, CodexSourceBackedErrorV0>;

impl From<CodexSourceBackedErrorV0> for CaptureError {
    fn from(error: CodexSourceBackedErrorV0) -> Self {
        match error {
            CodexSourceBackedErrorV0::Capture(error) => error,
            CodexSourceBackedErrorV0::Io(error) => Self::Io(error),
            CodexSourceBackedErrorV0::Json(error) => Self::Json(error),
            error => Self::InvalidPayload(error.to_string()),
        }
    }
}

#[cfg(any(test, ctx_codex_causal_qualification))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexSourceBackedCountersV0 {
    pub catalog_source_metadata_opens: u64,
    pub catalog_source_metadata_read_upper_bound_bytes: u64,
    pub catalog_session_meta_parses: u64,
    pub cold_sources: u64,
    pub appended_sources: u64,
    pub replaced_sources: u64,
    pub writer_exact_replay_sources: u64,
    pub writer_mutated_sources: u64,
    pub scanner_source_opens: u64,
    pub scanner_sources_started: u64,
    pub scanner_sources_completed: u64,
    pub staged_documents: u64,
    pub complete_records_scanned: u64,
    pub retained_records_scanned: u64,
    pub rejected_records_scanned: u64,
    pub ignored_records_scanned: u64,
    pub scanner_bytes_read: u64,
    pub mcp_terminal_authority_bytes_read: u64,
    pub structural_json_parses: u64,
    pub typed_json_parses: u64,
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
            catalog_source_metadata_opens,
            catalog_source_metadata_read_upper_bound_bytes,
            catalog_session_meta_parses,
            cold_sources,
            appended_sources,
            replaced_sources,
            writer_exact_replay_sources,
            writer_mutated_sources,
            scanner_sources_started,
            scanner_sources_completed,
            scanner_source_opens,
            staged_documents,
            complete_records_scanned,
            retained_records_scanned,
            rejected_records_scanned,
            ignored_records_scanned,
            scanner_bytes_read,
            mcp_terminal_authority_bytes_read,
            structural_json_parses,
            typed_json_parses,
        );
    }

    pub(crate) fn add_catalog_work(&mut self, work: CodexCatalogWorkV0) {
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
        self.mcp_terminal_authority_bytes_read = self
            .mcp_terminal_authority_bytes_read
            .saturating_add(scan.mcp_terminal_authority_bytes_read);
        self.structural_json_parses = self
            .structural_json_parses
            .saturating_add(scan.structural_json_parses);
        self.typed_json_parses = self
            .typed_json_parses
            .saturating_add(scan.typed_json_parses);
    }
}

mod catalog;
#[cfg(any(test, ctx_codex_causal_qualification))]
mod causal;
mod generation;
mod identity;
mod jsonl_family;
#[cfg(test)]
pub(crate) use catalog::install_after_codex_metadata_inventory_hook;
#[cfg(any(test, ctx_codex_causal_qualification))]
use catalog::CodexCatalogWorkV0;
pub(crate) use catalog::{
    absolute_lexical_path, codex_session_root_rank,
    observe_codex_explicit_session_source_backed_v0, CodexExplicitSessionSourceBackedInputV0,
};
#[cfg(test)]
pub(crate) use causal::{install_after_codex_causal_stage_hook_v1, CodexCausalSourceObservationV1};
pub(crate) use generation::{CodexGenerationNormalizationCoordinatorV0, CodexGenerationRouteV0};
pub(in crate::provider::codex::nativepath) use identity::{
    codex_core_record, codex_session_identity, codex_source_key, CodexEventIdentityStateV0,
};
pub(crate) use jsonl_family::CodexSessionJsonlFamilyAdapterV0;
