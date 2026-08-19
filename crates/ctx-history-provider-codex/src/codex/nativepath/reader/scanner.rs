use super::*;
use crate::provider::source_backed::ProviderRuntimeBinding;

impl CodexNativeScanner {
    pub(in crate::codex::nativepath) fn new_semantic(
        source: CodexCatalogSource,
        base_event_lookup: Option<impl crate::provider::source_backed::BaseEventLookup + 'static>,
    ) -> Result<Self> {
        let native_session_id = source.catalog_native_session_id.as_deref().ok_or_else(|| {
            CaptureError::from(CodexSourceBackedErrorV0::MissingNativeSessionId {
                path: source.source_path.clone(),
            })
        })?;
        let core_source = codex_source_key(native_session_id)?;
        let core_session_id = codex_session_identity(&core_source, native_session_id)?;
        Ok(Self {
            source,
            owner: None,
            session_metadata: Vec::new(),
            pending_calls: BTreeMap::new(),
            terminal_authority: CodexTerminalAuthority::default(),
            counters: CodexScanCounters::default(),
            local_turn_started: false,
            core_source,
            core_session_id,
            event_identity_state: base_event_lookup
                .map(CodexEventIdentityStateV0::for_append)
                .unwrap_or_default(),
            active_core_page: None,
            exhausted: false,
        })
    }

    pub(in crate::codex::nativepath) fn restore_semantic_checkpoint(
        &mut self,
        checkpoint: &super::super::checkpoint::CodexSemanticCheckpoint,
    ) -> Result<()> {
        if !checkpoint.direct_append_safe()
            || self.owner.is_some()
            || !self.session_metadata.is_empty()
            || !self.pending_calls.is_empty()
            || !self
                .terminal_authority
                .restore(checkpoint.terminal_authority())
        {
            return Err(CaptureError::InvalidPayload(
                "Codex semantic checkpoint cannot resume this scanner".to_owned(),
            ));
        }
        self.owner = checkpoint.owner().cloned();
        if let Some(owner) = self.owner.clone() {
            self.session_metadata.push(owner);
        }
        self.local_turn_started = checkpoint.local_turn_started();
        self.pending_calls.clone_from(checkpoint.pending_calls());
        Ok(())
    }

    pub(in crate::codex::nativepath) fn preflight_semantic(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<bool> {
        let direct_append = input.is_direct_append_resume();
        while let Some(record) = input.next_record()? {
            if !record.complete() {
                break;
            }
            if record.oversized() {
                self.terminal_authority.saturate();
            } else if !record.terminal_nul_padding() {
                self.terminal_authority
                    .observe_record(input.record_bytes(record)?);
            }
        }
        Ok(direct_append && self.terminal_authority.append_requires_replacement())
    }

    pub(in crate::codex::nativepath) fn next_semantic_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<Option<CodexNativePage>> {
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_semantic_page(input)?);
        }

        loop {
            let input_offset = input.offset()?;
            let page_start = self.active_semantic_page()?.expected_offset;
            let page_progress = input_offset.checked_sub(page_start).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex semantic physical page progress regressed".to_owned(),
                )
            })?;
            let page = self.active_semantic_page()?;
            let core_is_full = page.records.len() >= MAX_CODEX_PAGE_UNITS
                || page.serialized_bytes > MAX_CODEX_PAGE_BYTES
                || page.physical_records >= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS
                || page_progress >= MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES;
            if core_is_full {
                return self.emit_active_semantic_page().map(Some);
            }

            let position = self.semantic_position(input)?;
            let Some(record) = input.next_record()? else {
                self.exhausted = true;
                return self.emit_semantic_end_page();
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
                return self.emit_semantic_end_page();
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
                    CodexPhysicalRecordContext {
                        raw_ordinal: record.physical_ordinal(),
                        start_byte: record.byte_start(),
                        end_byte: record.byte_end_exclusive(),
                    },
                )?
            };

            let page = self.active_semantic_page()?;
            let (record_units, record_bytes) = match projection.context_mutation.as_ref() {
                Some(CodexContextMutation::SourceBackedRow {
                    estimated_bytes, ..
                }) => (1, *estimated_bytes),
                None => (0, 0),
            };
            let next_units = page.records.len().saturating_add(record_units);
            let next_bytes = page.serialized_bytes.saturating_add(record_bytes);
            let next_byte_limit = if page.records.is_empty() && record_units == 1 {
                MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
            } else {
                MAX_CODEX_PAGE_BYTES
            };
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > next_byte_limit {
                if page.physical_records != 0 {
                    self.restore_semantic(input, position)?;
                    return self.emit_active_semantic_page().map(Some);
                }
                self.reject(false);
                projection = CodexRecordProjection::default();
            } else {
                self.active_semantic_page()?.serialized_bytes = next_bytes;
            }
            if let Some(mutation) = projection.context_mutation.take() {
                self.apply_context_mutation(mutation)?;
            }
            let page = self.active_semantic_page()?;
            page.physical_records = page.physical_records.saturating_add(1);
        }
    }
}
