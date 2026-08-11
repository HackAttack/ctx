use super::*;

impl CodexNativeScanner {
    pub(super) fn new_semantic_page(
        &mut self,
        input: &JsonlFamilyExecutionIo,
    ) -> Result<CodexNativePage> {
        let expected_offset = input.complete_prefix_end()?;
        Ok(CodexNativePage {
            owner: self.owner.clone(),
            expected_offset,
            source_backed_rows: Vec::new(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES,
            physical_records: 0,
        })
    }

    pub(super) fn take_ready_semantic_page(&mut self) -> Option<CodexNativePage> {
        self.ready_core_page.take()
    }

    pub(super) fn emit_active_semantic_page(
        &mut self,
        input: &JsonlFamilyExecutionIo,
    ) -> Result<CodexNativePage> {
        let page = self
            .active_core_page
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath has no active semantic page to emit",
            ))?;
        self.finish_semantic_page(input, page)
    }

    pub(super) fn queue_semantic_end_page(&mut self, input: &JsonlFamilyExecutionIo) -> Result<()> {
        if let Some(page) = self.active_core_page.take() {
            if page.has_progress() {
                self.ready_core_page = Some(self.finish_semantic_page(input, page)?);
            }
        }
        Ok(())
    }

    pub(in crate::provider::codex::nativepath) fn finish_semantic(
        mut self,
    ) -> Result<CodexSemanticScan> {
        if !self.exhausted || self.active_core_page.is_some() || self.ready_core_page.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Codex semantic scan must drain every owned page before checkpointing".to_owned(),
            ));
        }
        let checkpoint = self
            .owner
            .take()
            .map(|owner| {
                validate_catalog_owner(&self.source, owner.clone())?;
                CodexSemanticCheckpoint::new(
                    &self.tool_authorities.into_values().collect::<Vec<_>>(),
                    self.mcp_terminal_authority.checkpoint(),
                    self.repository_candidate_authority.checkpoint(),
                    owner,
                    self.local_turn_started,
                )
                .map_err(CaptureError::from)
            })
            .transpose()?;
        Ok(CodexSemanticScan {
            checkpoint,
            counters: self.counters,
        })
    }

    pub(super) fn semantic_position(
        &self,
        input: &JsonlFamilyExecutionIo,
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
        input: &mut JsonlFamilyExecutionIo,
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
        _input: &JsonlFamilyExecutionIo,
        mut page: CodexNativePage,
    ) -> Result<CodexNativePage> {
        page.owner = self
            .owner
            .clone()
            .map(|owner| validate_catalog_owner(&self.source, owner))
            .transpose()?;
        debug_assert!(page.physical_records <= MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS);
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(
            page.serialized_bytes <= MAX_CODEX_PAGE_BYTES
                || (page.source_backed_rows.len() == 1
                    && page.serialized_bytes <= MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES)
        );
        self.counters.emitted_pages = self.counters.emitted_pages.saturating_add(1);
        self.counters.peak_page_rows = self.counters.peak_page_rows.max(page.units());
        self.counters.peak_page_bytes = self.counters.peak_page_bytes.max(page.serialized_bytes);
        Ok(page)
    }
}
