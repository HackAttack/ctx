use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile, CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod checkpoint;
mod framing;
mod identity;
mod physical;
mod revalidation;
mod route;
mod single_file;

pub(crate) use checkpoint::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};
pub(crate) use ctx_history_jsonl::{
    fit_jsonl_mcp_exchange, jsonl_prefix_digest as prefix_digest, jsonl_terminal_call_id_digest,
    new_jsonl_prefix_hasher as new_prefix_hasher, ordered_pending_exchange_entries,
    remember_pending_exchange, restore_hash_pending_exchange_entries,
    restore_ordered_pending_exchange_entries, selected_content_fits as jsonl_selected_content_fits,
    sorted_pending_exchange_entries, take_pending_exchange, JsonlAppendOccurrenceState,
    JsonlCheckpoint, JsonlCheckpointedTerminalAuthority, JsonlFileObservation,
    JsonlMcpObservedEncodedBytes, JsonlOrderedAppendOccurrenceState, JsonlOversizedRecordPolicy,
    JsonlPage, JsonlPendingExchangeLookup, JsonlPendingExchangeRemember, JsonlPendingExchangeState,
    JsonlRecordEvidence, JsonlRecordRef, JsonlScanOutcome, JsonlSourceChange, JsonlSourceIdentity,
    JsonlTerminalAuthority, JsonlTerminalObservationRegion,
};
use framing::read_bounded_record_complete_sha256;
pub(crate) use framing::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_full_complete_and_prefix_sha256, read_bounded_record_unhashed,
    JsonlRecordFraming,
};
use identity::observe_metadata;
pub(crate) use identity::{retained_file_identity, JsonlFileIdentityPolicy};
pub(crate) use physical::{
    JsonlPhysicalDigest, JsonlPhysicalRecord, JsonlPhysicalStream, JsonlPhysicalStreamPosition,
};
use revalidation::hash_prefix;
#[cfg(test)]
pub(crate) use revalidation::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_jsonl_semantic_preflight_hook, set_after_second_jsonl_prefix_hash_hook,
};
pub(crate) use revalidation::{
    observe_opened_file, observe_opened_file_allow_append, revalidate_frozen_prefix,
    revalidate_frozen_prefix_sha256,
};
#[cfg(test)]
pub(crate) use route::{
    checkpoint_admitted_revision_for_test, full_family_checkpoint_frontier_contract_for_test,
    set_before_jsonl_terminal_physical_revalidation_hook,
};
pub(crate) use route::{
    jsonl_family_driver, provider_checkpoint_for_base, JsonlFamilyAdapter, JsonlFamilyAppendMode,
    JsonlFamilyBaseScope, JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition,
    JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
    JsonlFamilyMembershipObservation, JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRejectedLeaf,
    JsonlFamilyRootMissingMode, JsonlFamilySemanticExecutor, JsonlFamilySemanticPage,
    JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary, JsonlFamilyTerminalProof,
    JsonlFamilyWorkerContext,
};
pub(crate) use single_file::jsonl_single_file_inventory;
const PAGE_MAX_RECORDS: usize = 64;
const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct JsonlProbe {
    observation: JsonlFileObservation,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
}

impl JsonlProbe {
    pub(crate) fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub(crate) fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }
}

pub(crate) struct JsonlReader {
    identity: JsonlSourceIdentity,
    observation: JsonlFileObservation,
    source_file: Arc<OpenedProviderSourceFile>,
    reader: Option<BufReader<File>>,
    physical: Option<JsonlPhysicalStream>,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
    source_change: JsonlSourceChange,
    skip_scan: bool,
    unchanged_checkpoint: Option<JsonlCheckpoint>,
    finished: bool,
    outcome: Option<JsonlScanOutcome>,
    record_buffer: Vec<u8>,
    whole_record: bool,
    append_log: bool,
    bind_admitted_eof: bool,
    complete_prefix_ends_with_terminal_nul_padding: bool,
    semantic_append_resume: Option<JsonlSemanticAppendResume>,
    semantic_preflight_binding: Option<JsonlSemanticPreflightBinding>,
    oversized_record_policy: JsonlOversizedRecordPolicy,
}

struct JsonlSemanticAppendResume {
    previous: JsonlCheckpoint,
    admitted_eof_sha256: [u8; 32],
    position: Option<JsonlPhysicalStreamPosition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonlSemanticPreflightBinding {
    physical: physical::JsonlPhysicalPassBinding,
    complete_prefix_ends_with_terminal_nul_padding: bool,
}

impl JsonlReader {
    pub(crate) fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> Result<Self> {
        Self::open_with_record_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlRecordFraming::ordinary(),
        )
    }

    pub(crate) fn open_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
    ) -> Result<Self> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            record_framing,
            false,
            false,
            None,
            None,
        )
    }

    pub(crate) fn open_semantic_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        previous_admitted_eof_sha256: Option<[u8; 32]>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> Result<Self> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            record_framing,
            false,
            true,
            previous_admitted_eof_sha256,
            frozen_observation,
        )
    }

    pub(crate) fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
    ) -> Result<Self> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            None,
            JsonlRecordFraming::ordinary(),
            true,
            false,
            None,
            None,
        )
    }

    fn open_with_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
        whole_record: bool,
        bind_admitted_eof: bool,
        deferred_append_eof_sha256: Option<[u8; 32]>,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> Result<Self> {
        source_file.revalidate_same_object()?;
        let current_metadata = source_file.file().metadata()?;
        let current_observation = observe_metadata(
            identity.source_path(),
            source_file.file(),
            &current_metadata,
        )?;
        let mut file = source_file.reopen_same_object()?;
        if observe_metadata(identity.source_path(), &file, &file.metadata()?)?
            != current_observation
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let observation = match frozen_observation {
            Some(frozen) if frozen.admits_frozen_prefix_in(&current_observation) => frozen.clone(),
            Some(_) => return Err(CaptureError::SourceChangedDuringCapture),
            None => current_observation,
        };

        let mut prefix_hasher = new_prefix_hasher();
        let mut complete_prefix_end = 0_u64;
        let mut next_physical_ordinal = 0_u64;
        let mut source_change = if previous.is_some() {
            JsonlSourceChange::Replace
        } else {
            JsonlSourceChange::Cold
        };
        let mut skip_scan = false;
        let mut unchanged_checkpoint = None;
        let mut semantic_append_resume = None;

        if let Some(previous) = previous.filter(|checkpoint| checkpoint.supports(&identity)) {
            let previous_observation = previous.source_observation();
            let same_file = previous_observation.same_stable_file(&observation);
            if same_file
                && previous_observation.supports_exact_revalidation()
                && previous_observation == &observation
            {
                // Exact physical equality also proves an unfinished tail is
                // unchanged. Its complete prefix remains the certified
                // frontier, so no provider projection or publication work is
                // needed until the file itself changes.
                complete_prefix_end = previous.complete_prefix_end();
                next_physical_ordinal = previous.next_physical_ordinal();
                source_change = JsonlSourceChange::Unchanged;
                skip_scan = true;
                unchanged_checkpoint = Some(previous.clone());
            } else if same_file && observation.length() >= previous.complete_prefix_end() {
                if let Some(admitted_eof_sha256) = deferred_append_eof_sha256 {
                    source_change = JsonlSourceChange::Append;
                    semantic_append_resume = Some(JsonlSemanticAppendResume {
                        previous: previous.clone(),
                        admitted_eof_sha256,
                        position: None,
                    });
                } else {
                    let observed_prefix = hash_prefix(
                        &mut file,
                        previous.complete_prefix_end(),
                        new_prefix_hasher(),
                    )?;
                    if prefix_digest(&observed_prefix) == *previous.complete_prefix_sha256() {
                        prefix_hasher = observed_prefix;
                        complete_prefix_end = previous.complete_prefix_end();
                        next_physical_ordinal = previous.next_physical_ordinal();
                        if previous.terminal()
                            && observation.length() == previous.complete_prefix_end()
                        {
                            source_change = JsonlSourceChange::Unchanged;
                            skip_scan = true;
                            unchanged_checkpoint = Some(previous.clone());
                        } else {
                            source_change = JsonlSourceChange::Append;
                        }
                    }
                }
            }
        }

        if matches!(
            source_change,
            JsonlSourceChange::Cold | JsonlSourceChange::Replace
        ) {
            if let Some(probe) = probe {
                if probe.observation != observation {
                    if !probe.observation.admits_frozen_prefix_in(&observation) {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    revalidate_frozen_prefix(
                        identity.source_path(),
                        source_file.as_ref(),
                        &probe.observation,
                        probe.complete_prefix_end,
                        prefix_digest(&probe.prefix_hasher),
                    )?;
                }
                prefix_hasher = probe.prefix_hasher;
                complete_prefix_end = probe.complete_prefix_end;
                next_physical_ordinal = probe.next_physical_ordinal;
            }
        }
        let full_hasher = if semantic_append_resume.is_some() {
            Some(Sha256::new())
        } else {
            bind_admitted_eof
                .then(|| hash_prefix(&mut file, complete_prefix_end, Sha256::new()))
                .transpose()?
        };
        file.seek(SeekFrom::Start(complete_prefix_end))?;
        let (reader, physical) = if whole_record {
            (Some(BufReader::new(file)), None)
        } else {
            (
                None,
                Some(JsonlPhysicalStream::open(
                    file,
                    observation.length(),
                    complete_prefix_end,
                    next_physical_ordinal,
                    record_framing,
                    match (full_hasher, semantic_append_resume.as_ref()) {
                        (Some(full), Some(resume)) => {
                            JsonlPhysicalDigest::full_complete_and_bounded_prefix(
                                full,
                                prefix_hasher.clone(),
                                Sha256::new(),
                                resume.previous.source_observation().length(),
                            )
                        }
                        (Some(full), None) => {
                            JsonlPhysicalDigest::full_and_complete(full, prefix_hasher.clone())
                        }
                        (None, _) => JsonlPhysicalDigest::complete(prefix_hasher.clone()),
                    },
                    || CaptureError::SourceChangedDuringCapture,
                )?),
            )
        };
        Ok(Self {
            identity,
            observation,
            source_file,
            reader,
            physical,
            prefix_hasher,
            complete_prefix_end,
            next_physical_ordinal,
            source_change,
            skip_scan,
            unchanged_checkpoint,
            finished: false,
            outcome: None,
            record_buffer: Vec::new(),
            whole_record,
            append_log: !whole_record,
            bind_admitted_eof,
            complete_prefix_ends_with_terminal_nul_padding: false,
            semantic_append_resume,
            semantic_preflight_binding: None,
            oversized_record_policy: JsonlOversizedRecordPolicy::RejectSource,
        })
    }

    pub(crate) fn set_oversized_record_policy(&mut self, policy: JsonlOversizedRecordPolicy) {
        self.oversized_record_policy = policy;
    }

    pub(crate) fn source_change(&self) -> JsonlSourceChange {
        self.source_change
    }

    pub(crate) fn outcome(&self) -> Option<&JsonlScanOutcome> {
        self.outcome.as_ref()
    }

    pub(super) fn next_execution_record(&mut self) -> Result<Option<JsonlPhysicalRecord>> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true)?;
            return Ok(None);
        }
        if self.whole_record {
            return Err(CaptureError::SystemInvariant(
                "whole-record JSON input cannot use the semantic executor",
            ));
        }
        if let Some(resume) = self.semantic_append_resume.as_mut() {
            let physical = self.physical.as_ref().ok_or(CaptureError::SystemInvariant(
                "semantic JSONL append lost its physical stream",
            ))?;
            let expected_end = resume.previous.complete_prefix_end();
            if resume.position.is_none() && physical.offset() == expected_end {
                if physical.next_physical_ordinal() == resume.previous.next_physical_ordinal()
                    && prefix_digest(physical.digest().complete_hasher())
                        == *resume.previous.complete_prefix_sha256()
                {
                    resume.position = Some(physical.position());
                }
            }
        }
        let record = self
            .physical
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .next_record()?;
        match record {
            None => self.finish(true)?,
            Some(record) if !record.complete => self.finish(false)?,
            Some(record) => {
                self.complete_prefix_ends_with_terminal_nul_padding = record.terminal_nul_padding;
            }
        }
        Ok(record)
    }

    pub(super) fn execution_record_bytes(&self, record: JsonlPhysicalRecord) -> Result<&[u8]> {
        Ok(self
            .physical
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .record_bytes(record))
    }

    pub(super) fn execution_position(&self) -> Result<JsonlPhysicalStreamPosition> {
        Ok(self
            .physical
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .position())
    }

    pub(super) fn restore_execution_position(
        &mut self,
        position: JsonlPhysicalStreamPosition,
    ) -> Result<()> {
        self.physical
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .restore(position)
    }

    pub(super) fn execution_offset(&self) -> Result<u64> {
        Ok(self
            .physical
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .offset())
    }

    pub(super) fn execution_complete_prefix_end(&self) -> Result<u64> {
        Ok(self
            .physical
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .complete_prefix_end())
    }

    pub(super) fn release_execution_record_buffer(&mut self) -> Result<()> {
        self.physical
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .release_record_buffer();
        Ok(())
    }

    pub(super) fn admitted_eof_sha256(&self) -> Result<Option<[u8; 32]>> {
        if !self.bind_admitted_eof {
            return Ok(None);
        }
        let full = self
            .physical
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "admitted-EOF JSONL input lost its physical stream",
            ))?
            .digest()
            .full_hasher()
            .ok_or(CaptureError::SystemInvariant(
                "admitted-EOF JSONL input lost its full digest",
            ))?;
        Ok(Some(full.clone().finalize().into()))
    }

    pub(super) fn complete_prefix_ends_with_terminal_nul_padding(&self) -> bool {
        self.complete_prefix_ends_with_terminal_nul_padding
    }

    pub(super) fn settle_semantic_preflight(
        &mut self,
        initial: JsonlPhysicalStreamPosition,
    ) -> Result<bool> {
        let binding = self.semantic_pass_binding()?;
        let restore = if let Some(resume) = self.semantic_append_resume.as_ref() {
            let physical = self.physical.as_ref().ok_or(CaptureError::SystemInvariant(
                "semantic JSONL append lost its physical stream",
            ))?;
            let Some((bounded_prefix, remaining)) = physical.digest().bounded_prefix() else {
                return Err(CaptureError::SystemInvariant(
                    "semantic JSONL append omitted its admitted-prefix digest",
                ));
            };
            let prefix_matches = remaining == 0
                && <[u8; 32]>::from(bounded_prefix.clone().finalize())
                    == resume.admitted_eof_sha256;
            let Some(position) = resume.position.clone() else {
                return Ok(false);
            };
            if !prefix_matches {
                return Ok(false);
            }
            position
        } else {
            initial
        };
        self.semantic_preflight_binding = Some(binding);
        #[cfg(test)]
        revalidation::run_after_jsonl_semantic_preflight_hook(self.identity.source_path());
        self.physical
            .as_mut()
            .ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?
            .restore(restore)?;
        self.finished = false;
        self.outcome = None;
        Ok(true)
    }

    fn semantic_pass_binding(&self) -> Result<JsonlSemanticPreflightBinding> {
        let physical = self.physical.as_ref().ok_or(CaptureError::SystemInvariant(
            "semantic JSONL input lost its physical stream",
        ))?;
        if !self.finished
            || self.outcome.is_none()
            || physical.offset() != self.observation.length()
        {
            return Err(CaptureError::SystemInvariant(
                "semantic JSONL pass was sealed before its admitted EOF",
            ));
        }
        Ok(JsonlSemanticPreflightBinding {
            physical: physical.admitted_pass_binding()?,
            complete_prefix_ends_with_terminal_nul_padding: self
                .complete_prefix_ends_with_terminal_nul_padding,
        })
    }

    pub(crate) fn visit_page<E>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), E>,
    ) -> std::result::Result<Option<JsonlPage>, E>
    where
        E: From<CaptureError>,
    {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true).map_err(E::from)?;
            return Ok(None);
        }
        if self.whole_record {
            return self.visit_whole_record(visit);
        }

        let mut records = 0_usize;
        let mut page_bytes = 0_usize;
        while records < PAGE_MAX_RECORDS {
            let (position, record) = {
                let physical = self.physical.as_mut().ok_or_else(|| {
                    E::from(CaptureError::SystemInvariant(
                        "ordinary JSONL source lost its physical stream",
                    ))
                })?;
                let position = physical.position();
                (position, physical.next_record().map_err(E::from)?)
            };
            let Some(record) = record else {
                self.finish(true).map_err(E::from)?;
                break;
            };
            if !record.complete {
                self.finish(false).map_err(E::from)?;
                break;
            }
            self.complete_prefix_ends_with_terminal_nul_padding = record.terminal_nul_padding;
            let wire_bytes = usize::try_from(record.byte_len()).unwrap_or(usize::MAX);
            let stored_record_bytes = {
                let record_bytes = self
                    .physical
                    .as_ref()
                    .ok_or_else(|| {
                        E::from(CaptureError::SystemInvariant(
                            "ordinary JSONL source lost its physical stream",
                        ))
                    })?
                    .record_bytes(record);
                record_bytes
                    .strip_suffix(b"\r")
                    .unwrap_or(record_bytes)
                    .len()
            };
            let oversized = record.oversized || stored_record_bytes > MAX_PROVIDER_JSONL_LINE_BYTES;
            if oversized && self.oversized_record_policy != JsonlOversizedRecordPolicy::RejectRecord
            {
                return Err(E::from(CaptureError::InvalidPayload(format!(
                    "{}:{} exceeds the {} byte JSONL record limit",
                    self.identity.source_path().display(),
                    record.physical_ordinal.saturating_add(1),
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }

            if records != 0 && page_bytes.saturating_add(wire_bytes) > PAGE_MAX_BYTES {
                self.physical
                    .as_mut()
                    .ok_or_else(|| {
                        E::from(CaptureError::SystemInvariant(
                            "ordinary JSONL source lost its physical stream",
                        ))
                    })?
                    .restore(position)
                    .map_err(E::from)?;
                break;
            }

            let evidence = JsonlRecordEvidence::new(
                record.physical_ordinal,
                record.byte_start,
                record.byte_end_exclusive,
                record.sha256,
            );
            let record_bytes = self
                .physical
                .as_ref()
                .ok_or_else(|| {
                    E::from(CaptureError::SystemInvariant(
                        "ordinary JSONL source lost its physical stream",
                    ))
                })?
                .record_bytes(record);
            let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
            visit(JsonlRecordRef::new(record_bytes, evidence, oversized))?;
            records = records.saturating_add(1);
            page_bytes = page_bytes.saturating_add(wire_bytes);
        }

        if records == 0 {
            return Ok(None);
        }
        Ok(Some(JsonlPage))
    }

    fn visit_whole_record<E>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), E>,
    ) -> std::result::Result<Option<JsonlPage>, E>
    where
        E: From<CaptureError>,
    {
        if self.complete_prefix_end != 0 || self.next_physical_ordinal != 0 {
            return Err(E::from(CaptureError::InvalidPayload(
                "whole-record JSON source has a non-empty scan frontier".to_owned(),
            )));
        }
        if self.observation.length() == 0 {
            self.finish(true).map_err(E::from)?;
            return Ok(None);
        }
        let length = usize::try_from(self.observation.length()).map_err(|_| {
            E::from(CaptureError::InvalidPayload(
                "whole-record JSON source exceeds platform limits".to_owned(),
            ))
        })?;
        if length > MAX_PROVIDER_JSONL_LINE_BYTES {
            return Err(E::from(CaptureError::InvalidPayload(format!(
                "{} exceeds the {} byte whole-record JSON limit",
                self.identity.source_path().display(),
                MAX_PROVIDER_JSONL_LINE_BYTES
            ))));
        }
        self.record_buffer.resize(length, 0);
        self.reader
            .as_mut()
            .ok_or_else(|| {
                E::from(CaptureError::SystemInvariant(
                    "whole-record JSON source lost its reader",
                ))
            })?
            .read_exact(&mut self.record_buffer)
            .map_err(CaptureError::from)
            .map_err(E::from)?;
        self.prefix_hasher.update(&self.record_buffer);
        let evidence = JsonlRecordEvidence::new(
            0,
            0,
            self.observation.length(),
            Sha256::digest(&self.record_buffer).into(),
        );
        visit(JsonlRecordRef::new(&self.record_buffer, evidence, false))?;
        self.complete_prefix_end = self.observation.length();
        self.next_physical_ordinal = 1;
        self.finish(true).map_err(E::from)?;
        Ok(Some(JsonlPage))
    }

    fn checkpoint(&self, terminal: bool) -> JsonlCheckpoint {
        let (complete_prefix_end, complete_prefix_sha256, next_physical_ordinal) =
            match self.physical.as_ref() {
                Some(physical) => (
                    physical.complete_prefix_end(),
                    prefix_digest(physical.digest().complete_hasher()),
                    physical.next_physical_ordinal(),
                ),
                None => (
                    self.complete_prefix_end,
                    prefix_digest(&self.prefix_hasher),
                    self.next_physical_ordinal,
                ),
            };
        JsonlCheckpoint::new(
            self.identity.clone(),
            self.observation.clone(),
            complete_prefix_end,
            complete_prefix_sha256,
            next_physical_ordinal,
            terminal,
        )
    }

    fn finish(&mut self, terminal: bool) -> Result<()> {
        if let Some(expected) = self.semantic_preflight_binding.as_ref() {
            let physical = self.physical.as_ref().ok_or(CaptureError::SystemInvariant(
                "semantic JSONL input lost its physical stream",
            ))?;
            if physical.terminal() != terminal {
                return Err(CaptureError::SystemInvariant(
                    "semantic JSONL terminal state disagreed with physical framing",
                ));
            }
            let actual = JsonlSemanticPreflightBinding {
                physical: physical.admitted_pass_binding()?,
                complete_prefix_ends_with_terminal_nul_padding: self
                    .complete_prefix_ends_with_terminal_nul_padding,
            };
            if &actual != expected {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        }
        let checkpoint = self.checkpoint(terminal);
        let current = observe_metadata(
            self.identity.source_path(),
            self.source_file.file(),
            &self.source_file.file().metadata()?,
        )?;
        if current == self.observation {
            if self.append_log {
                // The retained authority may have been opened before an
                // identity probe observed a legitimate append. The scan is
                // bound to `self.observation`, so require that exact
                // observation plus same-object routing rather than the
                // authority handle's older, metadata-sensitive stamp.
                self.source_file.revalidate_same_object()?;
            } else {
                self.source_file.revalidate()?;
            }
        } else {
            if !self.append_log {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            revalidate_frozen_prefix(
                self.identity.source_path(),
                self.source_file.as_ref(),
                &self.observation,
                checkpoint.complete_prefix_end(),
                *checkpoint.complete_prefix_sha256(),
            )?;
        }
        self.outcome = Some(JsonlScanOutcome::new(
            self.unchanged_checkpoint.clone().unwrap_or(checkpoint),
        ));
        self.finished = true;
        Ok(())
    }
}

/// Projects the first complete physical record and returns its prefix state.
///
/// Cold and replacement scans resume after this record, so the provider parser
/// sees every physical record at most once. Append and unchanged scans discard
/// the probe state after binding identity.
pub(crate) fn probe_first_record<T, E>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile>,
    visit: impl FnOnce(JsonlRecordRef<'_>) -> std::result::Result<T, E>,
) -> std::result::Result<(T, JsonlProbe), E>
where
    E: From<CaptureError>,
{
    let mut visit = Some(visit);
    probe_records_until(source_path, source_file, 1, |record| {
        visit.take().ok_or_else(|| {
            E::from(CaptureError::SystemInvariant(
                "provider identity probe visited more than one record",
            ))
        })?(record)
        .map(Some)
    })?
    .ok_or_else(|| {
        E::from(CaptureError::InvalidPayload(
            "provider identity record is missing or incomplete".to_owned(),
        ))
    })
}

pub(crate) fn probe_records_until<T, E>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile>,
    max_records: usize,
    mut visit: impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<(T, JsonlProbe)>, E>
where
    E: From<CaptureError>,
{
    if max_records == 0 || max_records > PAGE_MAX_RECORDS {
        return Err(E::from(CaptureError::SystemInvariant(
            "provider identity probe record bound is invalid",
        )));
    }
    source_file.revalidate_same_object().map_err(E::from)?;
    let observation = observe_metadata(
        source_path,
        source_file.file(),
        &source_file
            .file()
            .metadata()
            .map_err(CaptureError::from)
            .map_err(E::from)?,
    )
    .map_err(E::from)?;
    let mut file = source_file.reopen_same_object().map_err(E::from)?;
    file.seek(SeekFrom::Start(0))
        .map_err(CaptureError::from)
        .map_err(E::from)?;
    let mut reader = BufReader::new(file);
    let mut hasher = new_prefix_hasher();
    let mut buffer = Vec::new();
    let mut start = 0_u64;
    for ordinal in 0..max_records {
        let (end, record_digest, _wire_bytes) = match read_bounded_line(
            &mut reader,
            &mut buffer,
            &mut hasher,
            observation.length(),
            start,
        )
        .map_err(E::from)?
        {
            RawLine::Complete {
                end,
                record_digest,
                wire_bytes,
            } => (end, record_digest, wire_bytes),
            RawLine::EndOfFile | RawLine::IncompleteTail => break,
            RawLine::Oversized => {
                return Err(E::from(CaptureError::InvalidPayload(format!(
                    "provider identity record exceeds the {} byte JSONL record limit",
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }
        };
        let physical_ordinal = u64::try_from(ordinal).map_err(|_| {
            E::from(CaptureError::SystemInvariant(
                "provider identity probe ordinal exceeds u64",
            ))
        })?;
        if let Some(value) = visit(JsonlRecordRef::new(
            &buffer,
            JsonlRecordEvidence::new(physical_ordinal, start, end, record_digest),
            false,
        ))? {
            let closing = revalidate_frozen_prefix(
                source_path,
                source_file.as_ref(),
                &observation,
                end,
                prefix_digest(&hasher),
            )
            .map_err(E::from)?;
            return Ok(Some((
                value,
                JsonlProbe {
                    observation: closing,
                    prefix_hasher: hasher,
                    complete_prefix_end: end,
                    next_physical_ordinal: physical_ordinal.saturating_add(1),
                },
            )));
        }
        start = end;
    }
    revalidate_frozen_prefix(
        source_path,
        source_file.as_ref(),
        &observation,
        start,
        prefix_digest(&hasher),
    )
    .map_err(E::from)?;
    Ok(None)
}

enum RawLine {
    EndOfFile,
    IncompleteTail,
    Oversized,
    Complete {
        end: u64,
        record_digest: [u8; 32],
        wire_bytes: usize,
    },
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<RawLine> {
    bytes.clear();
    if start >= frozen_length {
        return Ok(RawLine::EndOfFile);
    }
    let Some(record) = read_bounded_record_complete_sha256(
        reader,
        bytes,
        hasher,
        frozen_length.saturating_sub(start),
        JsonlRecordFraming::ordinary(),
        || CaptureError::SourceChangedDuringCapture,
    )?
    else {
        return Ok(RawLine::EndOfFile);
    };
    if !record.complete {
        return Ok(RawLine::IncompleteTail);
    }
    let end = start
        .checked_add(record.byte_len)
        .ok_or(CaptureError::SystemInvariant(
            "JSONL byte offset overflowed",
        ))?;
    let wire_bytes = usize::try_from(record.byte_len).unwrap_or(usize::MAX);
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if record.oversized || bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        bytes.clear();
        return Ok(RawLine::Oversized);
    }
    Ok(RawLine::Complete {
        end,
        record_digest: record.sha256,
        wire_bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
    };

    use super::*;
    use crate::common::io::open_provider_source_file;

    fn drain(reader: &mut JsonlReader) -> Result<Vec<Vec<u8>>> {
        let mut records = Vec::new();
        while reader
            .visit_page(&mut |record| -> Result<()> {
                records.push(record.bytes().to_vec());
                Ok(())
            })?
            .is_some()
        {}
        Ok(records)
    }

    fn semantic_identity(source_path: &Path, revision: &str) -> JsonlSourceIdentity {
        JsonlSourceIdentity::new(
            "test",
            revision,
            "semantic-pass-binding-policy-v1",
            [9; 32],
            source_path.to_owned(),
        )
    }

    fn finish_semantic_pass(reader: &mut JsonlReader) -> Result<Vec<JsonlPhysicalRecord>> {
        let mut records = Vec::new();
        while let Some(record) = reader.next_execution_record()? {
            records.push(record);
        }
        Ok(records)
    }

    #[test]
    fn readers_opened_from_one_retained_source_drain_independently() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        fs::write(
            &source_path,
            b"{\"message\":\"first\"}\n{\"message\":\"second\"}\n",
        )
        .unwrap();
        let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
        let identity = JsonlSourceIdentity::new(
            "test",
            "independent-reader-v1",
            "independent-reader-policy-v1",
            [7; 32],
            source_path,
        );
        let mut first =
            JsonlReader::open(identity.clone(), Arc::clone(&source_file), None, None).unwrap();
        let mut second = JsonlReader::open(identity, source_file, None, None).unwrap();
        let expected = vec![
            br#"{"message":"first"}"#.to_vec(),
            br#"{"message":"second"}"#.to_vec(),
        ];

        assert_eq!(drain(&mut first).unwrap(), expected);
        assert_eq!(drain(&mut second).unwrap(), expected);
    }

    #[test]
    fn semantic_projection_rejects_same_length_rewrite_after_preflight() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        let admitted = b"{\"message\":\"authority-a\"}\n{\"message\":\"stable-z\"}\n";
        let rewritten = b"{\"message\":\"projected-b\"}\n{\"message\":\"stable-z\"}\n";
        assert_eq!(admitted.len(), rewritten.len());
        fs::write(&source_path, admitted).unwrap();
        let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
        let mut reader = JsonlReader::open_semantic_with_record_framing(
            semantic_identity(&source_path, "semantic-rewrite-v1"),
            source_file,
            None,
            None,
            None,
            JsonlRecordFraming::ordinary(),
            None,
        )
        .unwrap();

        let initial = reader.execution_position().unwrap();
        finish_semantic_pass(&mut reader).unwrap();
        let hook_path = source_path.clone();
        set_after_jsonl_semantic_preflight_hook(source_path, move || {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(hook_path)
                .unwrap();
            file.write_all(rewritten).unwrap();
            file.sync_all().unwrap();
        });
        assert!(reader.settle_semantic_preflight(initial).unwrap());

        let mut projected = Vec::new();
        let error = loop {
            match reader.next_execution_record() {
                Ok(Some(record)) => {
                    projected.push(reader.execution_record_bytes(record).unwrap().to_vec());
                }
                Ok(None) => {
                    panic!("rewritten projection unexpectedly satisfied the preflight seal")
                }
                Err(error) => break error,
            }
        };
        assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
        assert_eq!(
            projected,
            vec![
                br#"{"message":"projected-b"}"#.to_vec(),
                br#"{"message":"stable-z"}"#.to_vec()
            ]
        );
        assert!(reader.outcome().is_none());
    }

    #[test]
    fn semantic_binding_preserves_incomplete_tail_completion_ordinal() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        fs::write(&source_path, b"first\npartial").unwrap();
        let identity = semantic_identity(&source_path, "semantic-tail-v1");
        let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
        let mut first = JsonlReader::open_semantic_with_record_framing(
            identity.clone(),
            source_file,
            None,
            None,
            None,
            JsonlRecordFraming::ordinary(),
            None,
        )
        .unwrap();

        let initial = first.execution_position().unwrap();
        finish_semantic_pass(&mut first).unwrap();
        let hook_path = source_path.clone();
        set_after_jsonl_semantic_preflight_hook(source_path.clone(), move || {
            let mut file = OpenOptions::new().append(true).open(hook_path).unwrap();
            file.write_all(b"-done\n").unwrap();
            file.sync_all().unwrap();
        });
        assert!(first.settle_semantic_preflight(initial).unwrap());
        let first_records = finish_semantic_pass(&mut first).unwrap();
        assert_eq!(first_records.len(), 2);
        assert_eq!(first_records[1].physical_ordinal, 1);
        assert!(!first_records[1].complete);
        assert_eq!(
            first
                .outcome()
                .unwrap()
                .checkpoint()
                .next_physical_ordinal(),
            1
        );
        let checkpoint = first.outcome().unwrap().checkpoint().clone();
        let admitted_eof_sha256 = first.admitted_eof_sha256().unwrap().unwrap();
        drop(first);

        let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
        let mut resumed = JsonlReader::open_semantic_with_record_framing(
            identity,
            source_file,
            Some(&checkpoint),
            Some(admitted_eof_sha256),
            None,
            JsonlRecordFraming::ordinary(),
            None,
        )
        .unwrap();
        let preflight_start = resumed.execution_position().unwrap();
        finish_semantic_pass(&mut resumed).unwrap();
        assert!(resumed.settle_semantic_preflight(preflight_start).unwrap());
        let completed = resumed.next_execution_record().unwrap().unwrap();
        assert_eq!(completed.physical_ordinal, 1);
        assert!(completed.complete);
        assert_eq!(
            resumed.execution_record_bytes(completed).unwrap(),
            b"partial-done"
        );
        assert!(resumed.next_execution_record().unwrap().is_none());
        assert_eq!(
            resumed
                .outcome()
                .unwrap()
                .checkpoint()
                .next_physical_ordinal(),
            2
        );
    }
}
