use super::*;
use crate::provider::source_backed::ProviderRuntimeBinding;

impl CodexNativeScanner {
    pub(super) fn new_semantic_page(
        &mut self,
        input: &JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<CodexNativePage> {
        let expected_offset = input.complete_prefix_end()?;
        Ok(CodexNativePage {
            expected_offset,
            records: Vec::new(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES,
            physical_records: 0,
        })
    }

    pub(super) fn active_semantic_page(&mut self) -> Result<&mut CodexNativePage> {
        self.active_core_page
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath lost its active semantic page",
            ))
    }

    pub(super) fn emit_active_semantic_page(&mut self) -> Result<CodexNativePage> {
        let page = self
            .active_core_page
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath has no active semantic page to emit",
            ))?;
        self.finish_semantic_page(page)
    }

    pub(super) fn emit_semantic_end_page(&mut self) -> Result<Option<CodexNativePage>> {
        let Some(page) = self
            .active_core_page
            .take()
            .filter(|page| page.physical_records != 0)
        else {
            return Ok(None);
        };
        self.finish_semantic_page(page).map(Some)
    }

    pub(in crate::codex::nativepath) fn finish_semantic(mut self) -> Result<CodexSemanticScan> {
        if !self.exhausted || self.active_core_page.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Codex semantic scan must drain every owned page before finishing".to_owned(),
            ));
        }
        if !self.ownership_quarantined {
            self.validate_session_metadata_owner()?;
        }
        let terminal_authority = self.terminal_authority.checkpoint();
        let checkpoint = super::super::checkpoint::CodexSemanticCheckpoint::from_state(
            super::super::checkpoint::CodexSemanticCheckpointState {
                owner: self.owner.as_ref(),
                local_turn_started: self.local_turn_started,
                pending_calls: &self.pending_calls,
                terminal_authority,
            },
        )?;
        Ok(CodexSemanticScan {
            checkpoint,
            counters: self.counters,
            record_rejections: self.record_rejections,
        })
    }

    pub(super) fn semantic_position(
        &self,
        input: &JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
    ) -> Result<SemanticScannerPosition> {
        Ok(SemanticScannerPosition {
            input: input.position()?,
            had_owner: self.owner.is_some(),
            counters: self.counters,
            local_turn_started: self.local_turn_started,
        })
    }

    pub(super) fn restore_semantic(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<impl ProviderRuntimeBinding>,
        position: SemanticScannerPosition,
    ) -> Result<()> {
        let actual_parse_counts = (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
        );
        input.restore(position.input)?;
        if !position.had_owner {
            self.owner = None;
        }
        self.counters = position.counters;
        self.local_turn_started = position.local_turn_started;
        (
            self.counters.prefiltered_records,
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
        ) = actual_parse_counts;
        Ok(())
    }

    pub(super) fn finish_semantic_page(
        &mut self,
        page: CodexNativePage,
    ) -> Result<CodexNativePage> {
        debug_assert!(page.physical_records <= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS);
        debug_assert!(page.records.len() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(
            page.serialized_bytes <= MAX_CODEX_PAGE_BYTES
                || (page.records.len() == 1
                    && page.serialized_bytes <= MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES)
        );
        self.counters.emitted_pages = self.counters.emitted_pages.saturating_add(1);
        self.counters.peak_page_rows = self.counters.peak_page_rows.max(page.records.len());
        self.counters.peak_page_bytes = self.counters.peak_page_bytes.max(page.serialized_bytes);
        Ok(page)
    }
}
