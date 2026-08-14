use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use sha2::{Digest, Sha256};

use ctx_history_capture_runtime::{CaptureLifecycleSink, SourceBackedRouteDriver};
use ctx_history_source_io::{
    MappedOpenedProviderSourceFile, MappedOpenedProviderSourcePath, MappedProviderSourceDirectory,
    MappedProviderSourceRoot, SourceIoError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod checkpoint;
mod framing;
mod identity;
mod physical;
mod revalidation;
mod route;
mod single_file;

#[allow(
    unused_imports,
    reason = "shared family modules consume this compatibility prelude"
)]
pub(crate) use crate::{
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
pub use checkpoint::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};

pub trait JsonlFamilyError:
    std::error::Error
    + From<std::io::Error>
    + From<serde_json::Error>
    + From<ctx_history_source_io::SourceIoError>
    + Send
    + Sync
    + 'static
{
    fn invalid_payload(detail: String) -> Self;
    fn system_invariant(detail: &'static str) -> Self;
    fn worker_panicked(worker: &'static str) -> Self;
    fn source_changed() -> Self;
    fn is_not_found(&self) -> bool;
    fn is_source_changed(&self) -> bool;
    fn is_resource_unavailable(&self) -> bool;
    fn is_internal(&self) -> bool;
    fn is_ignorable_membership_entry(&self) -> bool;
}

/// Static, provider-neutral configuration for one JSONL family integration.
///
/// Concrete storage, repository attribution, and route-control policy remain
/// in the integrating capture crate. The family monomorphizes over those
/// ports without error boxing or dynamic lifecycle dispatch.
pub trait JsonlFamilyRuntime: Send + Sync + 'static {
    type Error: JsonlFamilyError;
    type Lifecycle: CaptureLifecycleSink;
    type WorkerServices: Default + Send;
    type RouteControl: Clone + Send + Sync + 'static;

    fn begin_worker_leaf(services: &mut Self::WorkerServices);
}

pub type JsonlRuntimeError<R> = <R as JsonlFamilyRuntime>::Error;
pub type JsonlRuntimeLookup<R> =
    <<R as JsonlFamilyRuntime>::Lifecycle as CaptureLifecycleSink>::BaseLookup;
pub type JsonlRuntimeLifecycleError<R> =
    <<R as JsonlFamilyRuntime>::Lifecycle as CaptureLifecycleSink>::Error;
pub type JsonlRuntimeDriver<R> = SourceBackedRouteDriver<
    <R as JsonlFamilyRuntime>::Lifecycle,
    <R as JsonlFamilyRuntime>::RouteControl,
>;

impl JsonlFamilyError for SourceIoError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }

    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }

    fn worker_panicked(worker: &'static str) -> Self {
        Self::SystemInvariant(worker)
    }

    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }

    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::SystemIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }

    fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChangedDuringCapture)
            || matches!(
                self,
                Self::InvalidProviderTranscriptPath { reason, .. }
                    if *reason == "provider source changed while its authority handle was retained"
            )
    }

    fn is_resource_unavailable(&self) -> bool {
        matches!(self, Self::Io(_) | Self::SystemIo { .. }) && !self.is_not_found()
    }

    fn is_internal(&self) -> bool {
        matches!(self, Self::SystemInvariant(_))
    }

    fn is_ignorable_membership_entry(&self) -> bool {
        ctx_history_source_io::is_symlink_source_rejection(self)
            || ctx_history_source_io::is_non_regular_source_rejection(self)
    }
}

pub type JsonlResult<T, E> = std::result::Result<T, E>;
pub type OpenedProviderSourceFile<E> = MappedOpenedProviderSourceFile<E>;
pub type OpenedProviderSourcePath<E> = MappedOpenedProviderSourcePath<E>;
pub type ProviderSourceDirectory<E> = MappedProviderSourceDirectory<E>;
pub type ProviderSourceRoot<E> = MappedProviderSourceRoot<E>;

#[cfg(test)]
type CaptureError = SourceIoError;
#[cfg(test)]
type Result<T> = std::result::Result<T, CaptureError>;
use framing::read_bounded_record_complete_sha256;
pub use framing::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_full_complete_and_prefix_sha256, read_bounded_record_unhashed,
    JsonlRecordFraming,
};
use identity::observe_metadata;
pub use physical::{
    JsonlPhysicalDigest, JsonlPhysicalRecord, JsonlPhysicalStream, JsonlPhysicalStreamPosition,
};
use revalidation::hash_prefix;
pub use revalidation::revalidate_frozen_prefix;
pub(crate) use revalidation::revalidate_frozen_prefix_sha256;
#[cfg(feature = "test-support")]
pub use revalidation::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_jsonl_semantic_preflight_hook, set_after_second_jsonl_prefix_hash_hook,
};
pub use revalidation::{observe_opened_file, observe_opened_file_allow_append};
#[cfg(feature = "test-support")]
pub use route::{
    checkpoint_admitted_revision_for_test, set_before_jsonl_terminal_physical_revalidation_hook,
};
pub use route::{
    jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
    JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition, JsonlFamilyInventory,
    JsonlFamilyInventoryMode, JsonlFamilyLeaf, JsonlFamilyMembershipObservation,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode, JsonlFamilyProjector,
    JsonlFamilyPublication, JsonlFamilyRejectedLeaf, JsonlFamilyRootMissingMode,
    JsonlFamilySemanticExecutor, JsonlFamilySemanticPage, JsonlFamilySemanticPreflight,
    JsonlFamilySemanticSummary, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
};
pub use single_file::jsonl_single_file_inventory;
const PAGE_MAX_RECORDS: usize = 64;
const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct JsonlProbe {
    observation: JsonlFileObservation,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
}

impl JsonlProbe {
    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }
}

pub struct JsonlReader<E: JsonlFamilyError> {
    identity: JsonlSourceIdentity,
    observation: JsonlFileObservation,
    source_file: Arc<OpenedProviderSourceFile<E>>,
    reader: Option<BufReader<File>>,
    physical: Option<JsonlPhysicalStream<E>>,
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
    admitted_eof_sha256: Option<[u8; 32]>,
    position: Option<JsonlPhysicalStreamPosition>,
}

struct JsonlReaderFramingOptions<'a> {
    record_framing: JsonlRecordFraming,
    whole_record: bool,
    bind_admitted_eof: bool,
    deferred_append_eof_sha256: Option<Option<[u8; 32]>>,
    frozen_observation: Option<&'a JsonlFileObservation>,
}

pub enum JsonlSemanticPreflightMode {
    AdmittedEof(Option<[u8; 32]>),
    CompletePrefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonlSemanticPreflightBinding {
    physical: physical::JsonlPhysicalPassBinding,
    complete_prefix_ends_with_terminal_nul_padding: bool,
}

impl<E: JsonlFamilyError> JsonlReader<E> {
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_record_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlRecordFraming::ordinary(),
        )
    }

    pub fn open_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                record_framing,
                whole_record: false,
                bind_admitted_eof: false,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
            },
        )
    }

    pub fn open_semantic_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> JsonlResult<Self, E> {
        let (bind_admitted_eof, deferred_append_eof_sha256) = match mode {
            JsonlSemanticPreflightMode::AdmittedEof(previous) => (true, previous.map(Some)),
            JsonlSemanticPreflightMode::CompletePrefix => (false, Some(None)),
        };
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                record_framing,
                whole_record: false,
                bind_admitted_eof,
                deferred_append_eof_sha256,
                frozen_observation,
            },
        )
    }

    pub fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            None,
            JsonlReaderFramingOptions {
                record_framing: JsonlRecordFraming::ordinary(),
                whole_record: true,
                bind_admitted_eof: false,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
            },
        )
    }

    fn open_with_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        options: JsonlReaderFramingOptions<'_>,
    ) -> JsonlResult<Self, E> {
        let JsonlReaderFramingOptions {
            record_framing,
            whole_record,
            bind_admitted_eof,
            deferred_append_eof_sha256,
            frozen_observation,
        } = options;
        source_file.revalidate_same_object()?;
        let current_metadata = source_file.file().metadata()?;
        let current_observation = observe_metadata::<E>(
            identity.source_path(),
            source_file.file(),
            &current_metadata,
        )?;
        let mut file = source_file.reopen_same_object()?;
        if observe_metadata::<E>(identity.source_path(), &file, &file.metadata()?)?
            != current_observation
        {
            return Err(E::source_changed());
        }
        let observation = match frozen_observation {
            Some(frozen) if frozen.admits_frozen_prefix_in(&current_observation) => frozen.clone(),
            Some(_) => return Err(E::source_changed()),
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
                    let observed_prefix = hash_prefix::<E>(
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
                        return Err(E::source_changed());
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
        let full_hasher = if semantic_append_resume
            .as_ref()
            .is_some_and(|resume| resume.admitted_eof_sha256.is_some())
        {
            Some(Sha256::new())
        } else {
            bind_admitted_eof
                .then(|| hash_prefix::<E>(&mut file, complete_prefix_end, Sha256::new()))
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
                        (Some(full), Some(resume)) if resume.admitted_eof_sha256.is_some() => {
                            JsonlPhysicalDigest::full_complete_and_bounded_prefix(
                                full,
                                prefix_hasher.clone(),
                                Sha256::new(),
                                resume.previous.source_observation().length(),
                            )
                        }
                        (Some(full), _) => {
                            JsonlPhysicalDigest::full_and_complete(full, prefix_hasher.clone())
                        }
                        (None, _) => JsonlPhysicalDigest::complete(prefix_hasher.clone()),
                    },
                    E::source_changed,
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

    pub fn set_oversized_record_policy(&mut self, policy: JsonlOversizedRecordPolicy) {
        self.oversized_record_policy = policy;
    }

    pub fn source_change(&self) -> JsonlSourceChange {
        self.source_change
    }

    pub fn outcome(&self) -> Option<&JsonlScanOutcome> {
        self.outcome.as_ref()
    }

    pub(super) fn next_execution_record(&mut self) -> JsonlResult<Option<JsonlPhysicalRecord>, E> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true)?;
            return Ok(None);
        }
        if self.whole_record {
            return Err(E::system_invariant(
                "whole-record JSON input cannot use the semantic executor",
            ));
        }
        self.capture_semantic_append_position()?;
        let record = self
            .physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
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

    fn capture_semantic_append_position(&mut self) -> JsonlResult<(), E> {
        if let Some(resume) = self.semantic_append_resume.as_mut() {
            let physical = self.physical.as_ref().ok_or_else(|| {
                E::system_invariant("semantic JSONL append lost its physical stream")
            })?;
            let expected_end = resume.previous.complete_prefix_end();
            if resume.position.is_none()
                && physical.offset() == expected_end
                && physical.next_physical_ordinal() == resume.previous.next_physical_ordinal()
                && prefix_digest(physical.digest().complete_hasher())
                    == *resume.previous.complete_prefix_sha256()
            {
                resume.position = Some(physical.position());
            }
        }
        Ok(())
    }

    pub(super) fn execution_record_bytes(
        &self,
        record: JsonlPhysicalRecord,
    ) -> JsonlResult<&[u8], E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .record_bytes(record))
    }

    pub(super) fn execution_position(&self) -> JsonlResult<JsonlPhysicalStreamPosition, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .position())
    }

    pub(super) fn restore_execution_position(
        &mut self,
        position: JsonlPhysicalStreamPosition,
    ) -> JsonlResult<(), E> {
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .restore(position)
    }

    pub(super) fn execution_offset(&self) -> JsonlResult<u64, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .offset())
    }

    pub(super) fn execution_complete_prefix_end(&self) -> JsonlResult<u64, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .complete_prefix_end())
    }

    pub(super) fn execution_certified_prefix_end(&self) -> Option<u64> {
        self.semantic_append_resume
            .as_ref()
            .map(|resume| resume.previous.complete_prefix_end())
    }

    pub(super) fn release_execution_record_buffer(&mut self) -> JsonlResult<(), E> {
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .release_record_buffer();
        Ok(())
    }

    pub(super) fn admitted_eof_sha256(&self) -> JsonlResult<Option<[u8; 32]>, E> {
        if !self.bind_admitted_eof {
            return Ok(None);
        }
        let full = self
            .physical
            .as_ref()
            .ok_or_else(|| {
                E::system_invariant("admitted-EOF JSONL input lost its physical stream")
            })?
            .digest()
            .full_hasher()
            .ok_or_else(|| E::system_invariant("admitted-EOF JSONL input lost its full digest"))?;
        Ok(Some(full.clone().finalize().into()))
    }

    pub(super) fn complete_prefix_ends_with_terminal_nul_padding(&self) -> bool {
        self.complete_prefix_ends_with_terminal_nul_padding
    }

    pub(super) fn settle_semantic_preflight(
        &mut self,
        initial: JsonlPhysicalStreamPosition,
        resume_append: bool,
        retain_failed_preflight: bool,
    ) -> JsonlResult<bool, E> {
        let binding = self.semantic_pass_binding()?;
        let (restore, ready) = match self.semantic_append_resume.as_ref() {
            Some(resume) => {
                let prefix_matches = resume.admitted_eof_sha256.is_none_or(|expected| {
                    self.physical
                        .as_ref()
                        .and_then(|physical| physical.digest().bounded_prefix())
                        .is_some_and(|(digest, remaining)| {
                            remaining == 0
                                && <[u8; 32]>::from(digest.clone().finalize()) == expected
                        })
                });
                match (resume_append && prefix_matches, resume.position.clone()) {
                    (true, Some(position)) => (position, true),
                    _ if !retain_failed_preflight => return Ok(false),
                    _ => (initial, false),
                }
            }
            None => (initial, true),
        };
        self.semantic_preflight_binding = Some(binding);
        #[cfg(any(test, feature = "test-support"))]
        revalidation::run_after_jsonl_semantic_preflight_hook(self.identity.source_path());
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .restore(restore)?;
        self.finished = false;
        self.outcome = None;
        Ok(ready)
    }

    fn semantic_pass_binding(&self) -> JsonlResult<JsonlSemanticPreflightBinding, E> {
        let physical = self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?;
        if !self.finished
            || self.outcome.is_none()
            || physical.offset() != self.observation.length()
        {
            return Err(E::system_invariant(
                "semantic JSONL pass was sealed before its admitted EOF",
            ));
        }
        Ok(JsonlSemanticPreflightBinding {
            physical: physical.admitted_pass_binding(),
            complete_prefix_ends_with_terminal_nul_padding: self
                .complete_prefix_ends_with_terminal_nul_padding,
        })
    }

    pub fn visit_page<V>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), V>,
    ) -> std::result::Result<Option<JsonlPage>, V>
    where
        V: From<E>,
    {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true).map_err(V::from)?;
            return Ok(None);
        }
        if self.whole_record {
            return self.visit_whole_record(visit);
        }

        let mut records = 0_usize;
        let mut page_bytes = 0_usize;
        while records < PAGE_MAX_RECORDS {
            self.capture_semantic_append_position().map_err(V::from)?;
            let (position, record) = {
                let physical = self.physical.as_mut().ok_or_else(|| {
                    V::from(E::system_invariant(
                        "ordinary JSONL source lost its physical stream",
                    ))
                })?;
                let position = physical.position();
                (position, physical.next_record().map_err(V::from)?)
            };
            let Some(record) = record else {
                self.finish(true).map_err(V::from)?;
                break;
            };
            if !record.complete {
                self.finish(false).map_err(V::from)?;
                break;
            }
            self.complete_prefix_ends_with_terminal_nul_padding = record.terminal_nul_padding;
            let wire_bytes = usize::try_from(record.byte_len()).unwrap_or(usize::MAX);
            let stored_record_bytes = {
                let record_bytes = self
                    .physical
                    .as_ref()
                    .ok_or_else(|| {
                        V::from(E::system_invariant(
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
                return Err(V::from(E::invalid_payload(format!(
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
                        V::from(E::system_invariant(
                            "ordinary JSONL source lost its physical stream",
                        ))
                    })?
                    .restore(position)
                    .map_err(V::from)?;
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
                    V::from(E::system_invariant(
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

    fn visit_whole_record<V>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), V>,
    ) -> std::result::Result<Option<JsonlPage>, V>
    where
        V: From<E>,
    {
        if self.complete_prefix_end != 0 || self.next_physical_ordinal != 0 {
            return Err(V::from(E::invalid_payload(
                "whole-record JSON source has a non-empty scan frontier".to_owned(),
            )));
        }
        if self.observation.length() == 0 {
            self.finish(true).map_err(V::from)?;
            return Ok(None);
        }
        let length = usize::try_from(self.observation.length()).map_err(|_| {
            V::from(E::invalid_payload(
                "whole-record JSON source exceeds platform limits".to_owned(),
            ))
        })?;
        if length > MAX_PROVIDER_JSONL_LINE_BYTES {
            return Err(V::from(E::invalid_payload(format!(
                "{} exceeds the {} byte whole-record JSON limit",
                self.identity.source_path().display(),
                MAX_PROVIDER_JSONL_LINE_BYTES
            ))));
        }
        self.record_buffer.resize(length, 0);
        self.reader
            .as_mut()
            .ok_or_else(|| {
                V::from(E::system_invariant(
                    "whole-record JSON source lost its reader",
                ))
            })?
            .read_exact(&mut self.record_buffer)
            .map_err(E::from)
            .map_err(V::from)?;
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
        self.finish(true).map_err(V::from)?;
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

    fn finish(&mut self, terminal: bool) -> JsonlResult<(), E> {
        if let Some(expected) = self.semantic_preflight_binding.as_ref() {
            let physical = self.physical.as_ref().ok_or_else(|| {
                E::system_invariant("semantic JSONL input lost its physical stream")
            })?;
            if physical.terminal() != terminal {
                return Err(E::system_invariant(
                    "semantic JSONL terminal state disagreed with physical framing",
                ));
            }
            let actual = JsonlSemanticPreflightBinding {
                physical: physical.admitted_pass_binding(),
                complete_prefix_ends_with_terminal_nul_padding: self
                    .complete_prefix_ends_with_terminal_nul_padding,
            };
            if &actual != expected {
                return Err(E::source_changed());
            }
        }
        let checkpoint = self.checkpoint(terminal);
        let current = observe_metadata::<E>(
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
                return Err(E::source_changed());
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
pub fn probe_first_record<T, E, V>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile<E>>,
    visit: impl FnOnce(JsonlRecordRef<'_>) -> JsonlResult<T, V>,
) -> JsonlResult<(T, JsonlProbe), V>
where
    E: JsonlFamilyError,
    V: From<E>,
{
    let mut visit = Some(visit);
    probe_records_until(source_path, source_file, 1, |record| {
        visit.take().ok_or_else(|| {
            V::from(E::system_invariant(
                "provider identity probe visited more than one record",
            ))
        })?(record)
        .map(Some)
    })?
    .ok_or_else(|| {
        V::from(E::invalid_payload(
            "provider identity record is missing or incomplete".to_owned(),
        ))
    })
}

pub fn probe_records_until<T, E, V>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile<E>>,
    max_records: usize,
    mut visit: impl FnMut(JsonlRecordRef<'_>) -> JsonlResult<Option<T>, V>,
) -> JsonlResult<Option<(T, JsonlProbe)>, V>
where
    E: JsonlFamilyError,
    V: From<E>,
{
    if max_records == 0 || max_records > PAGE_MAX_RECORDS {
        return Err(V::from(E::system_invariant(
            "provider identity probe record bound is invalid",
        )));
    }
    source_file.revalidate_same_object().map_err(V::from)?;
    let observation = observe_metadata::<E>(
        source_path,
        source_file.file(),
        &source_file.file().metadata().map_err(E::from)?,
    )
    .map_err(V::from)?;
    let mut file = source_file.reopen_same_object().map_err(V::from)?;
    file.seek(SeekFrom::Start(0))
        .map_err(E::from)
        .map_err(V::from)?;
    let mut reader = BufReader::new(file);
    let mut hasher = new_prefix_hasher();
    let mut buffer = Vec::new();
    let mut start = 0_u64;
    for ordinal in 0..max_records {
        let (end, record_digest, _wire_bytes) = match read_bounded_line::<E>(
            &mut reader,
            &mut buffer,
            &mut hasher,
            observation.length(),
            start,
        )
        .map_err(V::from)?
        {
            RawLine::Complete {
                end,
                record_digest,
                wire_bytes,
            } => (end, record_digest, wire_bytes),
            RawLine::EndOfFile | RawLine::IncompleteTail => break,
            RawLine::Oversized => {
                return Err(V::from(E::invalid_payload(format!(
                    "provider identity record exceeds the {} byte JSONL record limit",
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }
        };
        let physical_ordinal = u64::try_from(ordinal).map_err(|_| {
            V::from(E::system_invariant(
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
            .map_err(V::from)?;
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
    .map_err(V::from)?;
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

fn read_bounded_line<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> JsonlResult<RawLine, E> {
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
        E::source_changed,
    )?
    else {
        return Ok(RawLine::EndOfFile);
    };
    if !record.complete {
        return Ok(RawLine::IncompleteTail);
    }
    let end = start
        .checked_add(record.byte_len)
        .ok_or_else(|| E::system_invariant("JSONL byte offset overflowed"))?;
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
    use ctx_history_source_io::open_provider_source_file_mapped as open_provider_source_file;

    fn drain(reader: &mut JsonlReader<CaptureError>) -> Result<Vec<Vec<u8>>> {
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

    fn finish_semantic_pass(
        reader: &mut JsonlReader<CaptureError>,
    ) -> Result<Vec<JsonlPhysicalRecord>> {
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
            JsonlSemanticPreflightMode::AdmittedEof(None),
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
        assert!(reader
            .settle_semantic_preflight(initial, true, false)
            .unwrap());

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
            JsonlSemanticPreflightMode::AdmittedEof(None),
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
        assert!(first
            .settle_semantic_preflight(initial, true, false)
            .unwrap());
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
            JsonlSemanticPreflightMode::AdmittedEof(Some(admitted_eof_sha256)),
            None,
            JsonlRecordFraming::ordinary(),
            None,
        )
        .unwrap();
        assert_eq!(
            resumed.execution_certified_prefix_end(),
            Some(checkpoint.complete_prefix_end())
        );
        let preflight_start = resumed.execution_position().unwrap();
        finish_semantic_pass(&mut resumed).unwrap();
        assert!(resumed
            .settle_semantic_preflight(preflight_start, true, false)
            .unwrap());
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
