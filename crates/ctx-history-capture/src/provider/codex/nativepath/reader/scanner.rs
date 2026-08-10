use std::collections::BTreeSet;

use super::project::{
    mcp_terminal_candidate_evidence, CodexMcpTerminalAuthority, CodexRepositoryCandidateAuthority,
};
use super::*;
use crate::provider::codex::nativepath::record::codex_record_class;

struct McpTerminalAuthorityPreflight {
    authority: CodexMcpTerminalAuthority,
    repository_candidate_authority: CodexRepositoryCandidateAuthority,
    bytes_read: u64,
    peak_record_bytes: usize,
}

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

fn preflight_mcp_terminal_authority(
    opened: &OpenedProviderSourceFile,
    start: u64,
    frozen_len: u64,
    mut authority: CodexMcpTerminalAuthority,
    mut repository_candidate_authority: CodexRepositoryCandidateAuthority,
    mut repository_candidate_cells: BTreeSet<String>,
) -> Result<McpTerminalAuthorityPreflight> {
    let mut reader = BufReader::new(opened.file().try_clone()?);
    reader.seek(SeekFrom::Start(start))?;
    let mut offset = start;
    let mut record_buffer = Vec::new();
    let mut peak_record_bytes = 0_usize;
    while offset < frozen_len {
        let Some(record_read) = read_bounded_record_unhashed(
            &mut reader,
            &mut record_buffer,
            frozen_len.saturating_sub(offset),
        )?
        else {
            break;
        };
        offset = offset
            .checked_add(record_read.byte_len)
            .ok_or(CaptureError::SystemInvariant(
                "Codex authority preflight offset exceeds u64",
            ))?;
        peak_record_bytes = peak_record_bytes.max(record_read.stored_len);
        if !record_read.complete {
            break;
        }
        if record_read.terminal_nul_padding {
            continue;
        }
        if record_read.oversized {
            // The bounded scanner cannot classify any complete oversized
            // provider record. Conservatively exhaust both terminal domains:
            // it may contain either an MCP terminal or a result terminal.
            authority.observe_ambiguous_terminal();
            repository_candidate_authority.observe_ambiguous_record();
            continue;
        }
        let record = trim_jsonl_terminator(&record_buffer[..record_read.stored_len]);
        if result_terminal_authority_is_ambiguous(record) {
            authority.observe_ambiguous_result_terminal();
            repository_candidate_authority.observe_ambiguous_record();
        }
        if let Some(evidence) = mcp_terminal_candidate_evidence(record) {
            authority.observe(&evidence);
        }
        let Some(probe) = observe_result_terminal_call_id(&mut authority, record) else {
            continue;
        };
        match probe.class {
            CodexRecordClass::Retained(super::super::record::CodexRetainedKind::ToolCall) => {
                let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
                    repository_candidate_authority.observe_ambiguous_record();
                    continue;
                };
                let Some(payload) = envelope.get("payload") else {
                    continue;
                };
                let Some((call_id, context)) =
                    crate::provider::codex::repository::repository_invocation_context(
                        payload,
                        // Candidate recognition is syntactic. Projection
                        // retains the measured cwd for actual attribution.
                        Some("/"),
                    )
                else {
                    continue;
                };
                if crate::provider::codex::repository::repository_result_candidate(&context)
                    || context
                        .continuation_cell_id
                        .as_ref()
                        .is_some_and(|cell_id| repository_candidate_cells.contains(cell_id))
                {
                    repository_candidate_authority.observe_candidate_call(&call_id);
                } else {
                    repository_candidate_authority.observe_call_if_candidate(&call_id);
                }
            }
            CodexRecordClass::ExcludedResult(_) => {
                let Some(call_id) = probe
                    .call_id
                    .as_deref()
                    .filter(|call_id| !call_id.is_empty())
                else {
                    continue;
                };
                if !repository_candidate_authority.observe_result_if_candidate(call_id) {
                    continue;
                }
                let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
                    repository_candidate_authority.observe_ambiguous_record();
                    continue;
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
    Ok(McpTerminalAuthorityPreflight {
        authority,
        repository_candidate_authority,
        bytes_read: offset.saturating_sub(start),
        peak_record_bytes,
    })
}

impl CodexNativeScanner {
    #[cfg(test)]
    pub(super) fn new(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        Self::new_retained(source, proof)
    }

    pub(super) fn new_retained(
        mut source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        let opened = open_codex_source_capability(&source)?;
        source.opened = Some(Arc::clone(&opened));
        if let Some(proof) = proof {
            proof.validate_source(&source)?;
            validate_checkpoint_catalog_owner(&source, proof.checkpoint.owner.clone())?;
        }

        let before = observed_opened_file(&source, &opened)?;
        source.catalog_observation = before.clone();
        let file = opened.file().try_clone()?;
        let mut reader = BufReader::new(file);
        let validated = if let Some(proof) = proof {
            if before.len < proof.checkpoint.observation.len {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is longer than the observed source",
                ));
            }
            Some(validate_checkpoint_source(
                &mut reader,
                &proof.checkpoint,
                before.len > proof.checkpoint.observation.len,
            )?)
        } else {
            None
        };

        if let (Some(proof), Some(validated)) = (
            proof.filter(|proof| proof.checkpoint.observation == before),
            validated.as_ref(),
        ) {
            let replay_owner =
                validate_checkpoint_catalog_owner(&source, proof.checkpoint.owner.clone())?;
            let incomplete_tail = proof
                .checkpoint
                .incomplete_tail()
                .map(|(byte_len, sha256)| CodexIncompleteTail {
                    raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                    start_byte: proof.checkpoint.complete_prefix_end(),
                    byte_len,
                    sha256,
                });
            let replay = CodexSourceScan {
                source: source.clone(),
                before_observation: before.clone(),
                after_observation: before.clone(),
                disposition: CodexParseDisposition::ObservationReplay,
                full_revision_sha256: proof.checkpoint.full_revision_sha256,
                complete_prefix_sha256: proof.checkpoint.complete_prefix_sha256,
                complete_prefix_end: proof.checkpoint.complete_prefix_end(),
                next_raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                owner: Some(replay_owner),
                pending_tool_authorities: proof.checkpoint.pending_tool_authorities().to_vec(),
                terminal_authority: proof.checkpoint.terminal_authority().clone(),
                repository_candidate_authority: proof
                    .checkpoint
                    .repository_candidate_authority()
                    .clone(),
                incomplete_tail,
                counters: CodexScanCounters {
                    bytes_read: validated.bytes_read,
                    checkpoint_validation_bytes: validated.bytes_read,
                    prefix_bytes_read: proof.checkpoint.complete_prefix_end(),
                    peak_line_buffer_bytes: CHECKPOINT_READ_BUFFER_BYTES
                        .min(usize::try_from(validated.bytes_read).unwrap_or(usize::MAX)),
                    ..CodexScanCounters::default()
                },
                local_turn_started: proof.checkpoint.local_turn_started(),
            };
            return Ok(Self {
                source,
                opened,
                before,
                physical: None,
                disposition: CodexParseDisposition::ObservationReplay,
                owner: replay.owner.clone(),
                tool_contexts: BTreeMap::new(),
                tool_authorities: BTreeMap::new(),
                continuations: BTreeMap::new(),
                mcp_terminal_authority: CodexMcpTerminalAuthority::from_checkpoint(
                    proof.checkpoint.terminal_authority(),
                ),
                repository_candidate_authority: CodexRepositoryCandidateAuthority::from_checkpoint(
                    proof.checkpoint.repository_candidate_authority(),
                ),
                incomplete_tail: None,
                counters: replay.counters,
                local_turn_started: proof.checkpoint.local_turn_started(),
                replay: Some(replay),
                active_core_page: None,
                ready_core_page: None,
                exhausted: true,
            });
        }

        // The strict checkpoint carries the bounded authority derived from the
        // certified prefix. Extend it from suffix bytes only. If those bytes
        // invalidate a positive claim already published for this same source,
        // reject append and let the caller replace only this source.
        let append_prefix = proof
            .filter(|proof| before.len > proof.checkpoint.observation.len)
            .map(|proof| {
                CodexMcpTerminalAuthority::from_checkpoint(proof.checkpoint.terminal_authority())
            });
        let repository_candidate_append_prefix = proof
            .filter(|proof| before.len > proof.checkpoint.observation.len)
            .map(|proof| {
                CodexRepositoryCandidateAuthority::from_checkpoint(
                    proof.checkpoint.repository_candidate_authority(),
                )
            });
        let authority_start = append_prefix
            .as_ref()
            .and_then(|_| proof.map(|proof| proof.checkpoint.complete_prefix_end()))
            .unwrap_or(0);
        let authority_preflight = preflight_mcp_terminal_authority(
            &opened,
            authority_start,
            before.len,
            append_prefix.clone().unwrap_or_default(),
            repository_candidate_append_prefix
                .clone()
                .unwrap_or_default(),
            validated
                .as_ref()
                .map(|validated| {
                    validated
                        .pending_continuations
                        .iter()
                        .filter_map(|(cell_id, origin_call_id)| {
                            (!origin_call_id.is_empty()
                                && validated
                                    .pending_tool_contexts
                                    .get(origin_call_id)
                                    .is_some_and(|context| {
                                        crate::provider::codex::repository::repository_result_candidate(
                                            context,
                                        )
                                    }))
                            .then(|| cell_id.clone())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )?;
        if append_prefix.as_ref().is_some_and(|prefix| {
            prefix.appended_suffix_invalidates(&authority_preflight.authority)
        }) {
            return Err(invalid_checkpoint_proof(
                "an appended terminal invalidates certified native call authority",
            ));
        }
        if repository_candidate_append_prefix
            .as_ref()
            .is_some_and(|prefix| {
                prefix.appended_suffix_invalidates(
                    &authority_preflight.repository_candidate_authority,
                )
            })
        {
            return Err(invalid_checkpoint_proof(
                "an appended terminal invalidates certified repository candidate authority",
            ));
        }
        let authority_entries = authority_preflight.authority.entry_count();
        let authority_bytes = authority_preflight.authority.estimated_owned_bytes();
        let repository_candidate_entries = authority_preflight
            .repository_candidate_authority
            .entry_count();
        let repository_candidate_bytes = authority_preflight
            .repository_candidate_authority
            .estimated_owned_bytes();

        let (
            disposition,
            owner,
            tool_contexts,
            tool_authorities,
            continuations,
            raw_ordinal,
            offset,
            complete_hasher,
            validation_bytes,
            local_turn_started,
        ) = match (proof, validated) {
            (Some(proof), Some(validated)) if before.len > proof.checkpoint.observation.len => {
                let ValidatedCheckpoint {
                    bytes_read,
                    complete_prefix_hasher,
                    complete_prefix_ends_with_terminal_nul_padding,
                    pending_tool_contexts: tool_contexts,
                    pending_tool_authorities: tool_authorities,
                    pending_continuations: continuations,
                } = validated;
                if complete_prefix_ends_with_terminal_nul_padding {
                    return Err(invalid_checkpoint_proof(
                        "terminal NUL padding is not an append boundary",
                    ));
                }
                (
                    CodexParseDisposition::AppendDelta,
                    Some(proof.checkpoint.owner.clone()),
                    tool_contexts,
                    tool_authorities,
                    continuations,
                    proof.checkpoint.next_raw_ordinal(),
                    proof.checkpoint.complete_prefix_end(),
                    complete_prefix_hasher,
                    bytes_read,
                    proof.checkpoint.local_turn_started(),
                )
            }
            (Some(_), Some(_)) => {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is neither an exact replay nor an append prefix",
                ));
            }
            (None, None) => (
                CodexParseDisposition::FullGeneration,
                None,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                0,
                0,
                Sha256::new(),
                0,
                false,
            ),
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "Codex checkpoint validation state is incomplete",
                ));
            }
        };

        let physical = JsonlPhysicalStream::open(
            opened.file().try_clone()?,
            before.len,
            offset,
            raw_ordinal,
            JsonlRecordFraming::terminal_nul_padded(crate::MAX_PROVIDER_JSONL_LINE_BYTES),
            JsonlPhysicalDigest::full_and_complete(complete_hasher.clone(), complete_hasher),
            source_changed_during_scan,
        )?;

        Ok(Self {
            source,
            opened,
            before,
            physical: Some(physical),
            disposition,
            owner,
            tool_contexts,
            tool_authorities,
            continuations,
            mcp_terminal_authority: authority_preflight.authority,
            repository_candidate_authority: authority_preflight.repository_candidate_authority,
            incomplete_tail: None,
            counters: CodexScanCounters {
                bytes_read: validation_bytes,
                checkpoint_validation_bytes: validation_bytes,
                prefix_bytes_read: offset,
                mcp_terminal_authority_bytes_read: authority_preflight.bytes_read,
                peak_mcp_terminal_authority_entries: authority_entries,
                peak_mcp_terminal_authority_bytes: authority_bytes,
                peak_repository_candidate_authority_entries: repository_candidate_entries,
                peak_repository_candidate_authority_bytes: repository_candidate_bytes,
                peak_line_buffer_bytes: authority_preflight.peak_record_bytes,
                ..CodexScanCounters::default()
            },
            local_turn_started,
            replay: None,
            active_core_page: None,
            ready_core_page: None,
            exhausted: false,
        })
    }

    pub(crate) fn next_page(&mut self) -> Result<Option<CodexNativeOwnedPage>> {
        if let Some(page) = self.take_ready_page() {
            return Ok(Some(page));
        }
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_core_page()?);
        }

        loop {
            let core_is_full = self.active_core_page.as_ref().is_some_and(|page| {
                page.units() >= MAX_CODEX_PAGE_UNITS
                    || page.serialized_bytes > MAX_CODEX_PAGE_BYTES
                    || page.physical_records >= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
                    || self
                        .physical
                        .as_ref()
                        .map(JsonlPhysicalStream::offset)
                        .unwrap_or(page.expected_frontier.complete_prefix_end)
                        .saturating_sub(page.expected_frontier.complete_prefix_end)
                        >= MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES
            });
            if core_is_full {
                return self.emit_active_core_page().map(Some);
            }

            let position = self.position()?;
            let record = self
                .physical
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its physical JSONL stream",
                ))?
                .next_record()?;
            let Some(record) = record else {
                self.exhausted = true;
                self.queue_end_pages(true)?;
                return Ok(self.take_ready_page());
            };

            self.counters.bytes_read = self.counters.bytes_read.saturating_add(record.byte_len());
            self.counters.peak_line_buffer_bytes =
                self.counters.peak_line_buffer_bytes.max(record.stored_len);

            if !record.complete {
                self.incomplete_tail = Some(CodexIncompleteTail {
                    raw_ordinal: record.physical_ordinal,
                    start_byte: record.byte_start,
                    byte_len: record.byte_len(),
                    sha256: record.sha256,
                });
                self.counters.incomplete_records =
                    self.counters.incomplete_records.saturating_add(1);
                if record.oversized {
                    self.counters.oversized_records =
                        self.counters.oversized_records.saturating_add(1);
                }
                self.exhausted = true;
                self.queue_end_pages(false)?;
                return Ok(self.take_ready_page());
            }

            self.counters.complete_records = self.counters.complete_records.saturating_add(1);
            let mut projection = if record.terminal_nul_padding {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                CodexRecordProjection::default()
            } else if record.oversized {
                self.reject(true);
                CodexRecordProjection::default()
            } else {
                let record_buffer = self
                    .physical
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its physical JSONL stream",
                    ))?
                    .take_record_buffer();
                let result = self.process_record(
                    &record_buffer[..record.stored_len],
                    record.physical_ordinal,
                    record.byte_start,
                    record.byte_end_exclusive,
                    record.sha256,
                );
                self.physical
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its physical JSONL stream",
                    ))?
                    .restore_record_buffer(record_buffer);
                result?
            };

            let page = self
                .active_core_page
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active Core page",
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
                    self.restore(position)?;
                    return self.emit_active_core_page().map(Some);
                }
                self.reject(false);
                projection = CodexRecordProjection::default();
            } else {
                let page = self
                    .active_core_page
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its active Core page",
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
                    "Codex NativePath lost its active Core page",
                ))?;
            page.physical_records = page.physical_records.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod terminal_authority_tests {
    use super::result_terminal_authority_is_ambiguous;

    #[test]
    fn duplicate_selector_cannot_hide_terminal_authority() {
        assert!(result_terminal_authority_is_ambiguous(
            br#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call","output":"hidden"},"payload":{"type":"message","role":"user","content":[]}}"#,
        ));
        assert!(!result_terminal_authority_is_ambiguous(
            br#"{"type":"response_item","payload":{"type":"message","role":"user","content":[]}}"#,
        ));
    }
}
