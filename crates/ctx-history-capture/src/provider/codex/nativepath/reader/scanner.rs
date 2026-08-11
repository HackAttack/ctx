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
) {
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
        }
        return;
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
                return;
            };
            let Some(payload) = envelope.get("payload") else {
                return;
            };
            let Some((call_id, context)) =
                crate::provider::codex::repository::repository_invocation_context(
                    payload,
                    Some("/"),
                )
            else {
                return;
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
                return;
            };
            if !repository_candidate_authority.contains_candidate(call_id) {
                return;
            }
            let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
                repository_candidate_authority.observe_ambiguous_record();
                return;
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
}

struct CodexSemanticPreflight {
    authority: CodexMcpTerminalAuthority,
    repository_candidate_authority: CodexRepositoryCandidateAuthority,
    tool_contexts: BTreeMap<String, CodexToolCallContext>,
    tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    continuations: BTreeMap<String, String>,
    bytes_read: u64,
    records_visited: u64,
    peak_record_bytes: usize,
    peak_repository_occurrence_cache_entries: usize,
    peak_repository_occurrence_cache_bytes: usize,
}

fn preflight_semantic_authority(
    input: &mut JsonlFamilyExecutionIo,
    checkpoint: Option<&CodexSemanticCheckpoint>,
) -> Result<CodexSemanticPreflight> {
    let mut authority = CodexMcpTerminalAuthority::default();
    let mut repository_candidate_authority = CodexRepositoryCandidateAuthority::default();
    let mut repository_occurrences = CodexRepositoryOccurrenceCache::default();
    let mut repository_candidate_cells = BTreeSet::new();
    let mut tool_contexts = BTreeMap::new();
    let mut tool_authorities = BTreeMap::new();
    let pending_by_span = checkpoint
        .map(|checkpoint| {
            checkpoint
                .pending_tool_authorities()
                .iter()
                .map(|authority| {
                    (
                        (
                            authority.record_start,
                            authority.record_end,
                            authority.raw_ordinal,
                        ),
                        authority,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut bytes_read = 0_u64;
    let mut records_visited = 0_u64;
    let mut peak_record_bytes = 0_usize;
    while let Some(record) = input.next_record()? {
        bytes_read = bytes_read.saturating_add(record.byte_len());
        peak_record_bytes = peak_record_bytes.max(record.stored_len());
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
        let bytes = trim_jsonl_terminator(input.record_bytes(record)?);
        observe_semantic_preflight_record(
            bytes,
            &mut authority,
            &mut repository_candidate_authority,
            &mut repository_occurrences,
            &mut repository_candidate_cells,
        );
        let key = (
            record.byte_start(),
            record.byte_end_exclusive(),
            record.physical_ordinal(),
        );
        if let (Some(checkpoint), Some(pending)) = (checkpoint, pending_by_span.get(&key)) {
            let (call_id, context) = decode_pending_tool_authority(
                bytes,
                pending,
                checkpoint.owner(),
                checkpoint.local_turn_started(),
            )?;
            if tool_contexts.insert(call_id.clone(), context).is_some()
                || tool_authorities
                    .insert(call_id, (*pending).clone())
                    .is_some()
            {
                return Err(invalid_checkpoint_proof(
                    "pending tool-call authority correlation is duplicated",
                ));
            }
        }
    }
    if tool_authorities.len() != pending_by_span.len() {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority span is absent from the shared physical replay",
        ));
    }
    repository_occurrences.apply_suffix_to(&mut repository_candidate_authority);
    let continuations = restore_pending_continuations(&mut tool_contexts, &tool_authorities)?;
    Ok(CodexSemanticPreflight {
        authority,
        repository_candidate_authority,
        tool_contexts,
        tool_authorities,
        continuations,
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
        checkpoint: Option<CodexSemanticCheckpoint>,
    ) -> Result<Self> {
        let owner = checkpoint
            .as_ref()
            .map(|checkpoint| {
                validate_checkpoint_catalog_owner(&source, checkpoint.owner().clone())
            })
            .transpose()?;
        let mcp_terminal_authority = checkpoint
            .as_ref()
            .map(|checkpoint| {
                CodexMcpTerminalAuthority::from_checkpoint(checkpoint.terminal_authority())
            })
            .unwrap_or_default();
        let repository_candidate_authority = checkpoint
            .as_ref()
            .map(|checkpoint| {
                CodexRepositoryCandidateAuthority::from_checkpoint(
                    checkpoint.repository_candidate_authority(),
                )
            })
            .unwrap_or_default();
        let local_turn_started = checkpoint
            .as_ref()
            .is_some_and(CodexSemanticCheckpoint::local_turn_started);
        Ok(Self {
            source,
            owner,
            tool_contexts: BTreeMap::new(),
            tool_authorities: BTreeMap::new(),
            continuations: BTreeMap::new(),
            mcp_terminal_authority,
            repository_candidate_authority,
            counters: CodexScanCounters::default(),
            local_turn_started,
            active_core_page: None,
            ready_core_page: None,
            exhausted: false,
        })
    }

    pub(in crate::provider::codex::nativepath) fn preflight_semantic(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        checkpoint: Option<&CodexSemanticCheckpoint>,
    ) -> Result<bool> {
        let prior_mcp = checkpoint.map(|checkpoint| {
            CodexMcpTerminalAuthority::from_checkpoint(checkpoint.terminal_authority())
        });
        let prior_repository = checkpoint.map(|checkpoint| {
            CodexRepositoryCandidateAuthority::from_checkpoint(
                checkpoint.repository_candidate_authority(),
            )
        });
        let preflight = preflight_semantic_authority(input, checkpoint)?;
        let retry = prior_mcp
            .as_ref()
            .is_some_and(|prefix| prefix.appended_suffix_invalidates(&preflight.authority))
            || prior_repository.as_ref().is_some_and(|prefix| {
                prefix.appended_suffix_invalidates(&preflight.repository_candidate_authority)
            });
        if retry {
            return Ok(true);
        }
        self.tool_contexts = preflight.tool_contexts;
        self.tool_authorities = preflight.tool_authorities;
        self.continuations = preflight.continuations;
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
