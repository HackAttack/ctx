use super::*;

impl CodexNativeScanner {
    pub(super) fn process_record(
        &mut self,
        record: &[u8],
        physical: CodexPhysicalRecordContext,
    ) -> Result<CodexRecordProjection> {
        let record = trim_jsonl_terminator(record);
        if record.iter().all(u8::is_ascii_whitespace) {
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            return Ok(CodexRecordProjection::default());
        }

        // Records Core never materializes are the bulk of a Codex rollout. The
        // prefilter answers from the raw bytes, so they never reach a parse,
        // an allocation, or a payload hash.
        if let CodexRecordAdmission::NoProjection(projection) = prefilter_codex_record(record) {
            self.counters.prefiltered_records = self.counters.prefiltered_records.saturating_add(1);
            self.project_without_parse(projection);
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_json_parses =
            self.counters.structural_json_parses.saturating_add(1);
        let Some(probe) = classify_codex_record(record)
            .ok()
            .filter(|probe| !probe.lineage_malformed())
            .or_else(|| classify_after_selector_ambiguity(record))
        else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };
        match probe.class {
            CodexRecordClass::DescendantActivity | CodexRecordClass::DescendantStarted => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::SessionMeta => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match parse_session_meta(record) {
                    Some(owner) if self.owner.is_none() => {
                        let owner = validate_catalog_owner(&self.source, owner)?;
                        self.owner = Some(owner);
                        return Ok(CodexRecordProjection::default());
                    }
                    Some(_) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                    }
                    None => self.reject(false),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::TurnContext => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match (self.owner.as_mut(), parse_turn_context(record)) {
                    (Some(owner), Some((cwd, turn_id))) => {
                        owner.cwd = Some(cwd);
                        self.local_turn_started |= turn_id.as_deref().is_some_and(|turn_id| {
                            valid_local_turn_boundary(&owner.native_session_id, turn_id)
                        });
                    }
                    (None, _) | (_, None) => self.reject(false),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Retained(kind) => {
                let Some(owner) = self.owner.as_ref() else {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                };
                self.counters.retained_json_parses =
                    self.counters.retained_json_parses.saturating_add(1);
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                let Some(retained) = parse_decoded_record(record, owner) else {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                };
                let mut built = match build_source_backed_event_row(
                    physical.raw_ordinal,
                    kind,
                    &retained,
                    record,
                )? {
                    Ok(built) => built,
                    Err(CodexRetainedNonMaterialized::ValidUnmaterializable) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                        return Ok(CodexRecordProjection::default());
                    }
                    Err(CodexRetainedNonMaterialized::Malformed) => {
                        self.reject(false);
                        return Ok(CodexRecordProjection::default());
                    }
                };
                built.row.session_cwd.clone_from(&owner.cwd);
                let row_bytes = built.row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                }
                let lexical_bytes = built.row.lexical_body.len();
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(lexical_bytes).unwrap_or(u64::MAX));
                Ok(CodexRecordProjection {
                    context_mutation: Some(CodexContextMutation::SourceBackedRow {
                        row: built.row,
                        estimated_bytes: row_bytes,
                    }),
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => {
                self.process_output(record, &probe, result_kind, physical)
            }
        }
    }

    /// Applies the counter-only projection the prefilter proved sufficient.
    ///
    /// The arm mirrors the ignored-record counter in the parsed path exactly.
    fn project_without_parse(&mut self, projection: CodexSkipProjection) {
        match projection {
            CodexSkipProjection::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            }
        }
    }

    pub(super) fn process_output(
        &mut self,
        record: &[u8],
        probe: &CodexRecordProbe<'_>,
        result_kind: CodexResultKind,
        physical: CodexPhysicalRecordContext,
    ) -> Result<CodexRecordProjection> {
        self.counters.native_result_records = self.counters.native_result_records.saturating_add(1);
        self.counters.native_result_record_bytes = self
            .counters
            .native_result_record_bytes
            .saturating_add(physical.end_byte.saturating_sub(physical.start_byte));

        let call_id = probe.call_id.as_deref();
        let Some(owner) = self.owner.clone() else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject(false);
            return Ok(CodexRecordProjection::default());
        };

        self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
        let decoded = parse_decoded_record(record, &owner);
        let decoded = decoded.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex output could not be decoded for complete Core publication",
        ))?;
        let result_content = decoded
            .payload
            .get("output")
            .or_else(|| decoded.payload.get("result"));
        let audit = audit_codex_record(record)?;
        let normalized_body = if audit.selector_ambiguous(SelectorGroup::Result) {
            "Codex tool result".to_owned()
        } else {
            match result_content.unwrap_or(&decoded.payload) {
                Value::String(value) => value.clone(),
                value => serde_json::to_string(value)?,
            }
        };
        let structured_content = (!audit.any_selector_ambiguous()).then(|| decoded.payload.clone());
        let core_row = build_source_backed_sparse_output_row(
            physical.raw_ordinal,
            provider_event_identity(&decoded.payload),
            call_id,
            occurred_at,
            result_kind,
            normalized_body,
            structured_content,
            result_content,
            record,
            &decoded.payload,
            owner.cwd.clone(),
        )?;
        let context_mutation = match core_row {
            Some(row) => {
                let row_bytes = row.estimated_owned_bytes().unwrap_or(usize::MAX);
                if row_bytes
                    > MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                        .saturating_sub(PAGE_FIXED_WIRE_BYTES)
                {
                    self.reject(false);
                    return Ok(CodexRecordProjection::default());
                }
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(row.lexical_body.len()).unwrap_or(u64::MAX));
                return Ok(CodexRecordProjection {
                    context_mutation: Some(CodexContextMutation::SourceBackedRow {
                        row,
                        estimated_bytes: row_bytes,
                    }),
                });
            }
            None => None,
        };
        Ok(CodexRecordProjection { context_mutation })
    }

    pub(super) fn apply_context_mutation(&mut self, mutation: CodexContextMutation) -> Result<()> {
        self.apply_context_mutation_inner(mutation, true)
    }

    fn apply_context_mutation_inner(
        &mut self,
        mutation: CodexContextMutation,
        emit_record: bool,
    ) -> Result<()> {
        match mutation {
            CodexContextMutation::SourceBackedRow {
                row,
                estimated_bytes: _,
            } => {
                if emit_record {
                    debug_assert!(self.active_core_page.is_some());
                    let owner = self.owner.as_ref().ok_or(CaptureError::SystemInvariant(
                        "Codex retained record has no session owner",
                    ))?;
                    let record = codex_core_record(
                        &self.core_source,
                        self.core_session_id,
                        owner,
                        row,
                        &mut self.event_identity_state,
                    )?;
                    if let Some(page) = self.active_core_page.as_mut() {
                        page.records.push(record);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn reject(&mut self, oversized: bool) {
        if oversized {
            self.counters.oversized_records = self.counters.oversized_records.saturating_add(1);
        } else {
            self.counters.malformed_records = self.counters.malformed_records.saturating_add(1);
        }
        self.counters.rejected_complete_records =
            self.counters.rejected_complete_records.saturating_add(1);
    }
}

fn valid_local_turn_boundary(native_session_id: &str, turn_id: &str) -> bool {
    let (Ok(native_session_id), Ok(turn_id)) = (
        uuid::Uuid::parse_str(native_session_id),
        uuid::Uuid::parse_str(turn_id),
    ) else {
        return false;
    };
    uuid_v7_unix_timestamp_ms(&native_session_id)
        .zip(uuid_v7_unix_timestamp_ms(&turn_id))
        .is_some_and(|(session_ms, turn_ms)| turn_ms > session_ms)
}

fn uuid_v7_unix_timestamp_ms(value: &uuid::Uuid) -> Option<u64> {
    (value.get_version_num() == 7).then_some((value.as_u128() >> 80) as u64)
}

#[cfg(test)]
mod tests;
