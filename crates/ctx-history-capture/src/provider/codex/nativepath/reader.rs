use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    checkpoint::{
        CodexPendingToolAuthority, CodexRepositoryCandidateAuthorityCheckpoint,
        CodexRepositoryCandidateAuthorityEntry, CodexSemanticCheckpoint,
        CodexTerminalAuthorityCheckpoint, CodexTerminalAuthorityEntry,
        MAX_CODEX_CONTINUATION_CELL_ID_BYTES, MAX_CODEX_MCP_TERMINAL_AUTHORITIES,
        MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES, MAX_CODEX_TOOL_CALL_ID_BYTES,
        MAX_CODEX_TOOL_CONTEXTS,
    },
    record::{
        classify_codex_record, classify_mcp_terminal_after_selector_ambiguity,
        parse_decoded_record, parse_session_meta, parse_turn_context, prefilter_codex_record,
        CodexRecordAdmission, CodexRecordClass, CodexRecordProbe, CodexResultKind,
        CodexSkipProjection,
    },
    rows::{
        build_source_backed_event_row, build_source_backed_sparse_output_row, encoded_json_len,
        provider_event_identity, source_backed_display_text, source_backed_output_eligibility,
        CodexRetainedNonMaterialized, CodexSessionRow, CodexSourceBackedDocumentEligibility,
        CodexSourceBackedRowV0,
    },
    source::{CodexCatalogSource, CodexFileObservation},
};
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    provider::codex::events::{
        codex_exact_successful_function_output, codex_output_content, codex_result_value,
        CodexInvocationOriginV0, CodexToolCallContext,
    },
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
    },
    provider::source_backed::family::jsonl::{
        JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition,
    },
    CaptureError, Result,
};
const MAX_CODEX_PAGE_UNITS: usize = 64;
const MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS: u64 = 4 * 1024;
const MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES: u64 = 32 * 1024 * 1024;
const PAGE_FIXED_WIRE_BYTES: usize = 4 * 1024;
const MAX_CODEX_TOOL_NAME_BYTES: usize = 512;
const MAX_CODEX_TOOL_PREVIEW_BYTES: usize = 4 * 1024;

pub(crate) const MAX_CODEX_RECORD_BYTES: usize = 16 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MAX_CODEX_PAGE_ROWS: usize = MAX_CODEX_PAGE_UNITS;
pub(crate) const MAX_CODEX_PAGE_BYTES: usize = 8 * 1024 * 1024;
// One source-backed row may retain both decoded text and structured/path data
// derived from a single 16 MiB provider record. The ordinary page bound is a
// rollover target; this larger envelope is valid only for a singleton row.
pub(crate) const MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES: usize =
    PAGE_FIXED_WIRE_BYTES + (MAX_CODEX_RECORD_BYTES * 2) + (1024 * 1024);
// These stay wire-identical to provider_sources::ordinary_file so a catalog
// observation can be certified against identity read from the scanner's handle.
const ORDINARY_FILE_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const ORDINARY_FILE_FULL_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const ORDINARY_FILE_SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexScanCounters {
    pub(crate) bytes_read: u64,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) rejected_complete_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_record_bytes: u64,
    pub(crate) malformed_records: u64,
    pub(crate) oversized_records: u64,
    pub(crate) incomplete_records: u64,
    /// Records the pre-parse byte classifier answered without a structural parse.
    pub(crate) prefiltered_records: u64,
    /// Actual structural parse attempts, including a record retried after page rollback.
    pub(crate) structural_json_parses: u64,
    /// Actual typed parse attempts, including a record retried after page rollback.
    pub(crate) typed_json_parses: u64,
    pub(crate) structural_output_probes: u64,
    pub(crate) mcp_terminal_authority_bytes_read: u64,
    pub(crate) repository_candidate_authority_bytes_read: u64,
    pub(crate) repository_candidate_authority_records_visited: u64,
    pub(crate) peak_mcp_terminal_authority_entries: usize,
    pub(crate) peak_mcp_terminal_authority_bytes: usize,
    pub(crate) peak_repository_candidate_authority_entries: usize,
    pub(crate) peak_repository_candidate_authority_bytes: usize,
    pub(crate) peak_repository_occurrence_cache_entries: usize,
    pub(crate) peak_repository_occurrence_cache_bytes: usize,
    pub(crate) retained_json_parses: u64,
    pub(crate) retained_body_bytes: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) peak_page_rows: usize,
    pub(crate) peak_page_bytes: usize,
    pub(crate) peak_line_buffer_bytes: usize,
}

/// One owned, bounded Core page.
#[derive(Debug)]
pub(crate) struct CodexNativePage {
    pub(crate) owner: Option<CodexSessionRow>,
    expected_offset: u64,
    pub(crate) source_backed_rows: Vec<CodexSourceBackedRowV0>,
    pub(crate) serialized_bytes: usize,
    pub(crate) physical_records: u64,
}

impl CodexNativePage {
    fn units(&self) -> usize {
        self.source_backed_rows.len()
    }

    fn has_progress(&self) -> bool {
        self.physical_records != 0
    }
}

pub(super) struct CodexSemanticScan {
    pub(super) checkpoint: Option<CodexSemanticCheckpoint>,
    pub(super) counters: CodexScanCounters,
}

#[derive(Debug)]
pub(crate) struct CodexNativeScanner {
    source: CodexCatalogSource,
    owner: Option<CodexSessionRow>,
    tool_contexts: BTreeMap<String, CodexToolCallContext>,
    tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    continuations: BTreeMap<String, String>,
    mcp_terminal_authority: project::CodexMcpTerminalAuthority,
    repository_candidate_authority: project::CodexRepositoryCandidateAuthority,
    counters: CodexScanCounters,
    local_turn_started: bool,
    active_core_page: Option<CodexNativePage>,
    ready_core_page: Option<CodexNativePage>,
    exhausted: bool,
}

struct SemanticScannerPosition {
    input: JsonlFamilyExecutionPosition,
    had_owner: bool,
    counters: CodexScanCounters,
    local_turn_started: bool,
}

#[derive(Default)]
struct CodexRecordProjection {
    context_mutation: Option<CodexContextMutation>,
    source_backed_units: usize,
    core_serialized_bytes: usize,
}

impl CodexRecordProjection {
    fn core_units(&self) -> usize {
        self.source_backed_units
    }
}

// Produced once per decoded record: boxing the 296-byte source-backed mutation
// to match the 24-byte removal variant would add a per-record heap allocation.
#[allow(clippy::large_enum_variant)]
enum CodexContextMutation {
    Remove(Vec<String>),
    RegisterContinuation {
        cell_id: String,
        origin_call_id: String,
    },
    SourceBackedRow {
        row: CodexSourceBackedRowV0,
        insert_context: Option<(String, CodexToolCallContext, CodexPendingToolAuthority)>,
        remove_contexts: Vec<String>,
    },
}

mod checkpoint;
mod identity;
mod page_builder;
mod project;
mod scanner;

use checkpoint::*;
pub(crate) use checkpoint::{
    opened_file_observation as opened_codex_file_observation, opened_file_prefix_sha256,
    reopen_codex_source_capability, revalidate_codex_catalog_source_capability,
};
use identity::*;
