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
    read_bounded_record_unhashed, JsonlBoundedRecordRead, JsonlRecordFraming,
};
use identity::observe_metadata;
pub(crate) use identity::{retained_file_identity, JsonlFileIdentityPolicy};
pub(crate) use physical::{JsonlPhysicalDigest, JsonlPhysicalStream, JsonlPhysicalStreamPosition};
use revalidation::hash_prefix;
#[cfg(test)]
pub(crate) use revalidation::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_second_jsonl_prefix_hash_hook,
};
pub(crate) use revalidation::{
    observe_opened_file, observe_opened_file_allow_append, revalidate_frozen_prefix,
    revalidate_frozen_prefix_sha256,
};
#[cfg(test)]
pub(crate) use route::set_before_jsonl_terminal_physical_revalidation_hook;
pub(crate) use route::{
    jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
    JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
    JsonlFamilyMembershipObservation, JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRejectedLeaf,
    JsonlFamilyRootMissingMode, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
};
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
    oversized_record_policy: JsonlOversizedRecordPolicy,
}

impl JsonlReader {
    pub(crate) fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> Result<Self> {
        Self::open_with_framing(identity, source_file, previous, probe, false)
    }

    pub(crate) fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
    ) -> Result<Self> {
        Self::open_with_framing(identity, source_file, previous, None, true)
    }

    fn open_with_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        whole_record: bool,
    ) -> Result<Self> {
        source_file.revalidate_same_object()?;
        let current_metadata = source_file.file().metadata()?;
        let observation = observe_metadata(
            identity.source_path(),
            source_file.file(),
            &current_metadata,
        )?;
        let mut file = source_file.reopen_same_object()?;
        if observe_metadata(identity.source_path(), &file, &file.metadata()?)? != observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }

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
                let observed_prefix = hash_prefix(
                    &mut file,
                    previous.complete_prefix_end(),
                    new_prefix_hasher(),
                )?;
                if prefix_digest(&observed_prefix) == *previous.complete_prefix_sha256() {
                    prefix_hasher = observed_prefix;
                    complete_prefix_end = previous.complete_prefix_end();
                    next_physical_ordinal = previous.next_physical_ordinal();
                    if previous.terminal() && observation.length() == previous.complete_prefix_end()
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
                    JsonlRecordFraming::ordinary(),
                    JsonlPhysicalDigest::complete(prefix_hasher.clone()),
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
    use std::fs;

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
}
