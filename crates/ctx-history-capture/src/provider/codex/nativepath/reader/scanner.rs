use std::collections::BTreeSet;

use super::project::{
    mcp_terminal_candidate_evidence, CodexMcpTerminalAuthority, CodexRepositoryCandidateAuthority,
    CodexRepositoryOccurrenceCache,
};
use super::*;
use crate::provider::codex::nativepath::record::{
    classify_codex_repository_occurrence, codex_record_class,
};

fn result_terminal_authority_is_ambiguous(record: &[u8]) -> bool {
    // Codex treats a NUL-prefixed suffix as framing corruption, not a JSON
    // record candidate. Preserve that dedicated append-boundary diagnosis.
    if record.first() == Some(&0) {
        return false;
    }
    !crate::common::json::raw_object_keys_are_unique(record)
}

fn observe_result_terminal_call_id<'a>(
    authority: &mut CodexMcpTerminalAuthority,
    record: &'a [u8],
) -> Option<CodexRecordProbe<'a>> {
    if let Ok(probe) = classify_codex_record(record) {
        if matches!(probe.class, CodexRecordClass::ExcludedResult(_)) {
            if let Some(call_id) = probe
                .call_id
                .as_deref()
                .filter(|call_id| !call_id.is_empty())
            {
                authority.observe_result_call_id(call_id);
            }
        }
        return Some(probe);
    }

    // Projection can recover a bounded valid terminal after the strict
    // selector probe declines to expose linkage metadata. Observe that same
    // provider-recognized envelope here so uniqueness never depends on which
    // valid projection path retained the result.
    let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
        return None;
    };
    let Some(record_type) = envelope.get("type").and_then(Value::as_str) else {
        return None;
    };
    let Some(payload) = envelope.get("payload") else {
        return None;
    };
    let item_type = payload.get("type").and_then(Value::as_str);
    if !matches!(
        codex_record_class(record_type, item_type),
        CodexRecordClass::ExcludedResult(_)
    ) {
        return None;
    }
    if let Some(call_id) = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
    {
        authority.observe_result_call_id(call_id);
    }
    None
}

fn observe_repository_occurrence_cache(
    cache: &mut CodexRepositoryOccurrenceCache,
    class: CodexRecordClass,
    call_id: Option<&str>,
) {
    let Some(call_id) = call_id.filter(|call_id| !call_id.is_empty()) else {
        return;
    };
    match class {
        CodexRecordClass::Retained(super::super::record::CodexRetainedKind::ToolCall) => {
            cache.observe_call(call_id)
        }
        CodexRecordClass::ExcludedResult(_) => cache.observe_result(call_id),
        CodexRecordClass::SessionMeta
        | CodexRecordClass::TurnContext
        | CodexRecordClass::DescendantActivity
        | CodexRecordClass::DescendantStarted
        | CodexRecordClass::Retained(_)
        | CodexRecordClass::Ignored => {}
    }
}

fn observe_semantic_preflight_record(
    record: &[u8],
    authority: &mut CodexMcpTerminalAuthority,
    repository_candidate_authority: &mut CodexRepositoryCandidateAuthority,
    repository_occurrences: &mut CodexRepositoryOccurrenceCache,
    repository_candidate_cells: &mut BTreeSet<String>,
) -> Option<CodexRecordClass> {
    if result_terminal_authority_is_ambiguous(record) {
        authority.observe_ambiguous_result_terminal();
    }
    if let Some(evidence) = mcp_terminal_candidate_evidence(record) {
        authority.observe(&evidence);
    }
    let Some(probe) = observe_result_terminal_call_id(authority, record) else {
        if let Ok(probe) = classify_codex_repository_occurrence(record) {
            if probe.selector_malformed {
                repository_occurrences.observe_ambiguous_record();
            } else {
                observe_repository_occurrence_cache(
                    repository_occurrences,
                    probe.class,
                    probe.call_id.as_deref(),
                );
            }
            return Some(probe.class);
        }
        return None;
    };
    if probe.lineage_malformed() {
        repository_occurrences.observe_ambiguous_record();
    } else {
        observe_repository_occurrence_cache(
            repository_occurrences,
            probe.class,
            probe.call_id.as_deref(),
        );
    }
    match probe.class {
        CodexRecordClass::Retained(super::super::record::CodexRetainedKind::ToolCall) => {
            let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
                repository_candidate_authority.observe_ambiguous_record();
                return Some(probe.class);
            };
            let Some(payload) = envelope.get("payload") else {
                return Some(probe.class);
            };
            let Some((call_id, context)) =
                crate::provider::codex::repository::repository_invocation_context(
                    payload,
                    Some("/"),
                )
            else {
                return Some(probe.class);
            };
            if crate::provider::codex::repository::repository_result_candidate(&context)
                || context
                    .continuation_cell_id
                    .as_ref()
                    .is_some_and(|cell_id| repository_candidate_cells.contains(cell_id))
            {
                repository_candidate_authority.admit_candidate(&call_id);
            }
        }
        CodexRecordClass::ExcludedResult(_) => {
            let Some(call_id) = probe
                .call_id
                .as_deref()
                .filter(|call_id| !call_id.is_empty())
            else {
                return Some(probe.class);
            };
            if !repository_candidate_authority.contains_candidate(call_id) {
                return Some(probe.class);
            }
            let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
                repository_candidate_authority.observe_ambiguous_record();
                return Some(probe.class);
            };
            if let Some(cell_id) = envelope.get("payload").and_then(|payload| {
                crate::provider::codex::repository::running_continuation_cell_id(payload)
            }) {
                repository_candidate_cells.insert(cell_id);
            }
        }
        CodexRecordClass::SessionMeta
        | CodexRecordClass::TurnContext
        | CodexRecordClass::DescendantActivity
        | CodexRecordClass::DescendantStarted
        | CodexRecordClass::Retained(_)
        | CodexRecordClass::Ignored => {}
    }
    Some(probe.class)
}

struct CodexSemanticPreflight {
    prefix_authority: Option<CodexMcpTerminalAuthority>,
    authority: CodexMcpTerminalAuthority,
    prefix_repository_candidate_authority: Option<CodexRepositoryCandidateAuthority>,
    repository_candidate_authority: CodexRepositoryCandidateAuthority,
    bytes_read: u64,
    records_visited: u64,
    peak_record_bytes: usize,
    peak_repository_occurrence_cache_entries: usize,
    peak_repository_occurrence_cache_bytes: usize,
}

fn replay_semantic_prefix_record(
    scanner: &mut CodexNativeScanner,
    class: Option<CodexRecordClass>,
    bytes: &[u8],
    raw_ordinal: u64,
    start_byte: u64,
    end_byte: u64,
    record_digest: [u8; 32],
) -> Result<()> {
    if !matches!(
        class,
        Some(
            CodexRecordClass::SessionMeta
                | CodexRecordClass::TurnContext
                | CodexRecordClass::Retained(super::super::record::CodexRetainedKind::ToolCall)
                | CodexRecordClass::ExcludedResult(_)
        )
    ) {
        return Ok(());
    }
    let counters = scanner.counters;
    let mut projection =
        scanner.process_record(bytes, raw_ordinal, start_byte, end_byte, record_digest)?;
    if let Some(mutation) = projection.context_mutation.take() {
        scanner.apply_replayed_context_mutation(mutation);
    }
    scanner.counters = counters;
    Ok(())
}

fn certified_prefix_authority(
    authority: &CodexMcpTerminalAuthority,
    repository_candidate_authority: &CodexRepositoryCandidateAuthority,
    repository_occurrences: &CodexRepositoryOccurrenceCache,
) -> (CodexMcpTerminalAuthority, CodexRepositoryCandidateAuthority) {
    let mut repository_candidate_authority = repository_candidate_authority.clone();
    repository_occurrences.apply_suffix_to(&mut repository_candidate_authority);
    (authority.clone(), repository_candidate_authority)
}

fn preflight_semantic_authority(
    input: &mut JsonlFamilyExecutionIo,
    scanner: &mut CodexNativeScanner,
) -> Result<CodexSemanticPreflight> {
    let certified_prefix_end = input.certified_prefix_end();
    let mut certified_prefix_boundary_crossed = false;
    let mut prefix_authority = None;
    let mut prefix_repository_candidate_authority = None;
    let mut authority = CodexMcpTerminalAuthority::default();
    let mut repository_candidate_authority = CodexRepositoryCandidateAuthority::default();
    let mut repository_occurrences = CodexRepositoryOccurrenceCache::default();
    let mut repository_candidate_cells = BTreeSet::new();
    let mut bytes_read = 0_u64;
    let mut records_visited = 0_u64;
    let mut peak_record_bytes = 0_usize;
    while let Some(record) = input.next_record()? {
        bytes_read = bytes_read.saturating_add(record.byte_len());
        peak_record_bytes = peak_record_bytes.max(record.stored_len());
        if let Some(certified_prefix_end) = certified_prefix_end {
            if record.byte_start() < certified_prefix_end
                && record.byte_end_exclusive() > certified_prefix_end
            {
                certified_prefix_boundary_crossed = true;
            } else if !certified_prefix_boundary_crossed
                && prefix_authority.is_none()
                && record.byte_start() >= certified_prefix_end
            {
                let (mcp, repository) = certified_prefix_authority(
                    &authority,
                    &repository_candidate_authority,
                    &repository_occurrences,
                );
                prefix_authority = Some(mcp);
                prefix_repository_candidate_authority = Some(repository);
            }
        }
        if !record.complete() {
            break;
        }
        records_visited = records_visited.saturating_add(1);
        if record.terminal_nul_padding() {
            continue;
        }
        if record.oversized() {
            authority.observe_ambiguous_terminal();
            repository_occurrences.observe_ambiguous_record();
            continue;
        }
        let record_bytes = input.record_bytes(record)?;
        let bytes = trim_jsonl_terminator(record_bytes);
        let class = observe_semantic_preflight_record(
            bytes,
            &mut authority,
            &mut repository_candidate_authority,
            &mut repository_occurrences,
            &mut repository_candidate_cells,
        );
        if certified_prefix_end.is_some_and(|prefix_end| record.byte_end_exclusive() <= prefix_end)
        {
            scanner.mcp_terminal_authority = authority.clone();
            scanner.repository_candidate_authority = certified_prefix_authority(
                &authority,
                &repository_candidate_authority,
                &repository_occurrences,
            )
            .1;
            replay_semantic_prefix_record(
                scanner,
                class,
                record_bytes,
                record.physical_ordinal(),
                record.byte_start(),
                record.byte_end_exclusive(),
                record.sha256(),
            )?;
        }
    }
    if certified_prefix_end.is_some()
        && prefix_authority.is_none()
        && !certified_prefix_boundary_crossed
    {
        let (mcp, repository) = certified_prefix_authority(
            &authority,
            &repository_candidate_authority,
            &repository_occurrences,
        );
        prefix_authority = Some(mcp);
        prefix_repository_candidate_authority = Some(repository);
    }
    repository_occurrences.apply_suffix_to(&mut repository_candidate_authority);
    Ok(CodexSemanticPreflight {
        prefix_authority,
        authority,
        prefix_repository_candidate_authority,
        repository_candidate_authority,
        bytes_read,
        records_visited,
        peak_record_bytes,
        peak_repository_occurrence_cache_entries: repository_occurrences.peak_entry_count(),
        peak_repository_occurrence_cache_bytes: repository_occurrences.estimated_peak_owned_bytes(),
    })
}

impl CodexNativeScanner {
    pub(in crate::provider::codex::nativepath) fn new_semantic(
        source: CodexCatalogSource,
        _checkpoint: Option<CodexSemanticCheckpoint>,
    ) -> Result<Self> {
        Ok(Self {
            source,
            owner: None,
            tool_contexts: BTreeMap::new(),
            tool_authorities: BTreeMap::new(),
            continuations: BTreeMap::new(),
            mcp_terminal_authority: CodexMcpTerminalAuthority::default(),
            repository_candidate_authority: CodexRepositoryCandidateAuthority::default(),
            counters: CodexScanCounters::default(),
            local_turn_started: false,
            active_core_page: None,
            ready_core_page: None,
            exhausted: false,
        })
    }

    pub(in crate::provider::codex::nativepath) fn preflight_semantic(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        _checkpoint: Option<&CodexSemanticCheckpoint>,
    ) -> Result<bool> {
        let preflight = preflight_semantic_authority(input, self)?;
        let retry = preflight
            .prefix_authority
            .as_ref()
            .is_some_and(|prefix| prefix.appended_suffix_invalidates(&preflight.authority))
            || preflight
                .prefix_repository_candidate_authority
                .as_ref()
                .is_some_and(|prefix| {
                    prefix.appended_suffix_invalidates(&preflight.repository_candidate_authority)
                });
        if retry {
            return Ok(true);
        }
        self.mcp_terminal_authority = preflight.authority;
        self.repository_candidate_authority = preflight.repository_candidate_authority;
        self.counters.mcp_terminal_authority_bytes_read = preflight.bytes_read;
        self.counters.repository_candidate_authority_bytes_read = preflight.bytes_read;
        self.counters.repository_candidate_authority_records_visited = preflight.records_visited;
        self.counters.peak_line_buffer_bytes = preflight.peak_record_bytes;
        self.counters.peak_mcp_terminal_authority_entries =
            self.mcp_terminal_authority.entry_count();
        self.counters.peak_mcp_terminal_authority_bytes =
            self.mcp_terminal_authority.estimated_owned_bytes();
        self.counters.peak_repository_candidate_authority_entries =
            self.repository_candidate_authority.entry_count();
        self.counters.peak_repository_candidate_authority_bytes =
            self.repository_candidate_authority.estimated_owned_bytes();
        self.counters.peak_repository_occurrence_cache_entries =
            preflight.peak_repository_occurrence_cache_entries;
        self.counters.peak_repository_occurrence_cache_bytes =
            preflight.peak_repository_occurrence_cache_bytes;
        Ok(false)
    }

    pub(in crate::provider::codex::nativepath) fn next_semantic_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<Option<CodexNativePage>> {
        if let Some(page) = self.take_ready_semantic_page() {
            return Ok(Some(page));
        }
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_semantic_page(input)?);
        }

        loop {
            let input_offset = input.offset()?;
            let page_start = self
                .active_core_page
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active semantic page",
                ))?
                .expected_offset;
            let page_progress = input_offset.checked_sub(page_start).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex semantic physical page progress regressed".to_owned(),
                )
            })?;
            let core_is_full = self.active_core_page.as_ref().is_some_and(|page| {
                page.units() >= MAX_CODEX_PAGE_UNITS
                    || page.serialized_bytes > MAX_CODEX_PAGE_BYTES
                    || page.physical_records >= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
                    || page_progress >= MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES
            });
            if core_is_full {
                return self.emit_active_semantic_page(input).map(Some);
            }

            let position = self.semantic_position(input)?;
            let Some(record) = input.next_record()? else {
                self.exhausted = true;
                self.queue_semantic_end_page(input)?;
                return Ok(self.take_ready_semantic_page());
            };
            self.counters.bytes_read = self.counters.bytes_read.saturating_add(record.byte_len());
            self.counters.peak_line_buffer_bytes = self
                .counters
                .peak_line_buffer_bytes
                .max(record.stored_len());
            if !record.complete() {
                self.counters.incomplete_records =
                    self.counters.incomplete_records.saturating_add(1);
                if record.oversized() {
                    self.counters.oversized_records =
                        self.counters.oversized_records.saturating_add(1);
                }
                self.exhausted = true;
                self.queue_semantic_end_page(input)?;
                return Ok(self.take_ready_semantic_page());
            }

            self.counters.complete_records = self.counters.complete_records.saturating_add(1);
            let mut projection = if record.terminal_nul_padding() {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                CodexRecordProjection::default()
            } else if record.oversized() {
                self.reject(true);
                CodexRecordProjection::default()
            } else {
                self.process_record(
                    input.record_bytes(record)?,
                    record.physical_ordinal(),
                    record.byte_start(),
                    record.byte_end_exclusive(),
                    record.sha256(),
                )?
            };

            let page = self
                .active_core_page
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active semantic page",
                ))?;
            let next_units = page.units().saturating_add(projection.core_units());
            let next_bytes = page
                .serialized_bytes
                .saturating_add(projection.core_serialized_bytes);
            let next_byte_limit = if page.units() == 0 && projection.core_units() == 1 {
                MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
            } else {
                MAX_CODEX_PAGE_BYTES
            };
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > next_byte_limit {
                if page.has_progress() {
                    self.restore_semantic(input, position)?;
                    return self.emit_active_semantic_page(input).map(Some);
                }
                self.reject(false);
                projection = CodexRecordProjection::default();
            } else {
                let page = self
                    .active_core_page
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its active semantic page",
                    ))?;
                page.serialized_bytes = next_bytes;
            }
            if let Some(mutation) = projection.context_mutation.take() {
                self.apply_context_mutation(mutation);
            }
            let page = self
                .active_core_page
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active semantic page",
                ))?;
            page.physical_records = page.physical_records.saturating_add(1);
        }
    }
}
