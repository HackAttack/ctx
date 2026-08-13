use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
};

use ctx_history_index_generation::{
    lexical_index_settings, load_active_generation_pointer, DurableMmapDirectory,
};
use sha2::{Digest, Sha256};
use tantivy::{
    postings::Postings,
    schema::{Field, IndexRecordOption},
    termdict::TermMerger,
    tokenizer::TokenStream,
    DocAddress, DocSet, Executor, Index, InvertedIndexReader, ReloadPolicy, Searcher, Term,
    TERMINATED,
};
use uuid::Uuid;

#[cfg(any(test, feature = "test-support"))]
use std::cell::Cell;

use crate::{
    accumulate_core_record, core_record_accumulator_leaf, fields_from_schema, hex,
    load_publication_for_metas, meta_generation, open_slot_index, searcher_generation,
    stored_verification_identities, stored_verification_record, validate_schema,
    validate_verification_projection, verify_certified_physical_integrity,
    verify_or_certify_physical_integrity, ActiveGenerationPointer, CertifiedPhysicalIntegrity,
    CompactIdentity, Fields, GenerationManifest, GenerationSlot, IdentityFieldRole, IndexError,
    LoadedPublication, PhysicalIntegrityAudit, Result, VerificationRecord,
};

use super::{physical_integrity_audit, verify_physical_integrity};

mod lineage;
mod spill;

use lineage::{verify_incremental_lineage, SessionRelationship};
use spill::{
    reserve_verification_scratch, with_verification_scratch_budget, IdentityDeltaSpill,
    ProjectionAccumulator, ProjectionDeltas, ScratchReservation, SpillVerificationIdentities,
    VerificationSpill, VERIFICATION_SPILL_BUFFER_BYTES, VERIFICATION_SPILL_RECORD_BYTES,
};

#[derive(Default)]
struct SourceAggregate {
    count: u64,
    accumulator: [u8; 32],
}

struct SegmentVerification {
    document_count: u64,
    document_decodes: usize,
    stored_core_bytes: u64,
    body_tokens: u64,
    source_aggregates: BTreeMap<String, SourceAggregate>,
    parent_session_documents: u64,
}

#[derive(Clone, Copy)]
struct SegmentVerificationTask {
    segment_ord: usize,
    start_doc_id: u32,
    end_doc_id: u32,
}

#[derive(Default)]
struct VerificationCounters {
    active_workers: AtomicUsize,
    max_active_workers: AtomicUsize,
}

struct ActiveVerificationWorker<'a> {
    counters: Option<&'a VerificationCounters>,
}

impl<'a> ActiveVerificationWorker<'a> {
    fn enter(counters: Option<&'a VerificationCounters>) -> Self {
        if let Some(counters) = counters {
            let active = counters.active_workers.fetch_add(1, Ordering::SeqCst) + 1;
            counters
                .max_active_workers
                .fetch_max(active, Ordering::SeqCst);
        }
        Self { counters }
    }
}

impl Drop for ActiveVerificationWorker<'_> {
    fn drop(&mut self) {
        if let Some(counters) = self.counters {
            counters.active_workers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[derive(Default)]
struct VerificationRunMetrics {
    #[cfg(any(test, feature = "test-support"))]
    worker_budget: usize,
    segment_tasks: usize,
    document_decodes: usize,
    source_terms: usize,
    max_active_workers: usize,
    max_buffered_segments: usize,
    max_buffered_event_identities: usize,
    max_buffered_session_identities: usize,
    stored_core_bytes: u64,
    body_tokens: u64,
    verification_spill_bytes: u64,
    verification_tracked_heap_bytes: usize,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static LOGICAL_PASSES: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_TERMS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_DOCUMENTS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_PROJECTION_DOCUMENTS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_LINEAGE_DECODES: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_LINEAGE_SPILLS: Cell<usize> = const { Cell::new(0) };
    static COMPLETE_SESSION_ID_TRAVERSALS: Cell<usize> = const { Cell::new(0) };
}

pub fn verify_searcher_structure(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    let actual = searcher.num_docs();
    if actual != manifest.indexed_documents {
        return Err(IndexError::DocumentCountMismatch {
            manifest: manifest.indexed_documents,
            index: actual,
        });
    }
    Ok(())
}

pub fn verify_searcher(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    let worker_budget = verification_worker_budget(searcher.num_docs());
    verify_searcher_with_options(searcher, manifest, worker_budget, false, false).map(|_| ())
}

/// An immutable active publication loaded and structurally checked by the
/// format authority. Its private provenance is the only base accepted by the
/// incremental candidate verifier.
pub struct PinnedPublication {
    writer_index: Option<Index>,
    searcher: Searcher,
    manifest: GenerationManifest,
    generation_id: String,
    publication_metadata: Option<Arc<[u8]>>,
    fields: Fields,
    opstamp: u64,
    physical_integrity: CertifiedPhysicalIntegrity,
}

impl PinnedPublication {
    #[doc(hidden)]
    pub fn searcher(&self) -> &Searcher {
        &self.searcher
    }

    #[doc(hidden)]
    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    #[doc(hidden)]
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    #[doc(hidden)]
    pub fn publication_metadata(&self) -> Option<&Arc<[u8]>> {
        self.publication_metadata.as_ref()
    }

    #[doc(hidden)]
    pub fn into_writer_parts(mut self) -> Result<(Index, Fields, u64, Self)> {
        let index = self.writer_index.take().ok_or(IndexError::WriterInvariant(
            "pinned publication lost its writer index",
        ))?;
        Ok((index, self.fields, self.opstamp, self))
    }
}

/// Empty, payload-free state retained only for the writer's existing cold-root
/// compatibility path.
pub struct EmptyPublicationIndex {
    index: Index,
    fields: Fields,
    opstamp: u64,
}

impl EmptyPublicationIndex {
    #[doc(hidden)]
    pub fn into_parts(self) -> (Index, Fields, u64) {
        (self.index, self.fields, self.opstamp)
    }
}

/// Result of opening one writer base slot without exposing raw trust inputs.
// Keep the move-only Index inline: boxing this one-shot handoff would add a
// heap allocation to every compatible writer open.
#[allow(clippy::large_enum_variant)]
pub enum OpenedPinnedPublication {
    Published(PinnedPublication),
    Empty(EmptyPublicationIndex),
}

/// Opaque proof that a publication pointer was decoded from the durable root.
/// It prevents callers from promoting an arbitrary constructed slot into an
/// incremental-verification base.
pub struct ActivePublicationAuthority {
    pointer: ActiveGenerationPointer,
}

impl ActivePublicationAuthority {
    pub fn pointer(&self) -> &ActiveGenerationPointer {
        &self.pointer
    }

    #[doc(hidden)]
    pub fn into_pointer(self) -> ActiveGenerationPointer {
        self.pointer
    }
}

/// Loads and canonically validates the durable publication authority once.
pub fn load_active_publication_authority(
    root: &Path,
) -> Result<Option<ActivePublicationAuthority>> {
    load_active_generation_pointer(root)
        .map(|pointer| pointer.map(|pointer| ActivePublicationAuthority { pointer }))
        .map_err(Into::into)
}

/// Opens the exact durable active slot and captures its immutable
/// query/searcher provenance for later candidate or reuse trust. The opaque
/// publication is minted only after the pointer-bound physical certification
/// proves the active slot's expected SHA. The ordinary certified path checks
/// artifact identities without reading their bodies; a missing certification
/// performs and installs the one required expected-SHA scrub.
pub fn open_pinned_publication(
    root: &Path,
    authority: &ActivePublicationAuthority,
) -> Result<OpenedPinnedPublication> {
    let slot = authority.pointer.active();
    let index = open_slot_index(root, slot)?;
    validate_schema(&index.schema())?;
    let fields = fields_from_schema(&index.schema())?;
    let metas = index.load_metas()?;
    if metas.payload.is_none() {
        if metas.segments.is_empty() {
            return Ok(OpenedPinnedPublication::Empty(EmptyPublicationIndex {
                index,
                fields,
                opstamp: metas.opstamp,
            }));
        }
        return Err(IndexError::UnboundIndexState);
    }
    let publication = load_publication_for_metas(root, &metas)?;
    if slot.generation_id() != publication.generation_id() {
        return Err(IndexError::InvalidActiveGenerationPointer);
    }
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    if searcher_generation(&searcher) != meta_generation(&metas) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    verify_searcher_structure(&searcher, publication.manifest())?;
    let physical_integrity =
        verify_or_certify_physical_integrity(root, authority.pointer(), slot, searcher.index())?;
    let (generation_id, manifest, publication_metadata) = publication.into_parts();
    Ok(OpenedPinnedPublication::Published(PinnedPublication {
        writer_index: Some(index),
        searcher,
        manifest,
        generation_id,
        publication_metadata,
        fields,
        opstamp: metas.opstamp,
        physical_integrity,
    }))
}

/// Opaque authority proving that one immutable searcher and its publication
/// metadata passed the format-owned trust checks required by a query reader.
pub struct VerifiedPublication {
    searcher: Searcher,
    manifest: Arc<GenerationManifest>,
    generation_id: String,
    publication_metadata: Option<Arc<[u8]>>,
}

impl VerifiedPublication {
    /// Decomposes an already-verified publication for the query package.
    #[doc(hidden)]
    pub fn into_parts(self) -> (Searcher, Arc<GenerationManifest>, String, Option<Arc<[u8]>>) {
        (
            self.searcher,
            self.manifest,
            self.generation_id,
            self.publication_metadata,
        )
    }

    #[doc(hidden)]
    pub fn searcher(&self) -> &Searcher {
        &self.searcher
    }

    #[doc(hidden)]
    pub fn shared_manifest(&self) -> &Arc<GenerationManifest> {
        &self.manifest
    }

    #[doc(hidden)]
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
}

/// A completely verified writer candidate together with its one physical
/// audit, retained so activation certification does not hash it again.
pub struct VerifiedCandidatePublication {
    publication: VerifiedPublication,
    physical_integrity_audit: PhysicalIntegrityAudit,
}

impl VerifiedCandidatePublication {
    #[doc(hidden)]
    pub fn publication(&self) -> &VerifiedPublication {
        &self.publication
    }

    #[doc(hidden)]
    pub fn physical_integrity_audit(&self) -> &PhysicalIntegrityAudit {
        &self.physical_integrity_audit
    }

    #[doc(hidden)]
    pub fn into_publication(self) -> VerifiedPublication {
        self.publication
    }
}

/// One candidate whose searcher, commit metadata, manifest, and exact path
/// were opened together by the format authority. The borrowed path avoids an
/// allocation while preventing callers from cross-wiring independently valid
/// candidate components at the verifier boundary.
pub struct OpenedPublicationCandidate<'a> {
    index: Index,
    publication: LoadedPublication,
    generation_path: &'a Path,
    metas: tantivy::IndexMeta,
}

impl OpenedPublicationCandidate<'_> {
    pub fn generation_id(&self) -> &str {
        self.publication.generation_id()
    }

    #[doc(hidden)]
    pub fn metas(&self) -> &tantivy::IndexMeta {
        &self.metas
    }
}

/// Opens and binds every immutable component of an exact writer candidate.
pub fn open_publication_candidate<'a>(
    root: &Path,
    generation_path: &'a Path,
) -> Result<OpenedPublicationCandidate<'a>> {
    let directory =
        DurableMmapDirectory::open(generation_path).map_err(tantivy::TantivyError::from)?;
    let index = Index::open(directory)?;
    validate_schema(&index.schema())?;
    if index.settings() != &lexical_index_settings() {
        return Err(IndexError::IndexSettingsMismatch(
            crate::LEXICAL_SCHEMA_VERSION,
        ));
    }
    let metas = index.load_metas()?;
    let publication = load_publication_for_metas(root, &metas)?;
    Ok(OpenedPublicationCandidate {
        index,
        publication,
        generation_path,
        metas,
    })
}

/// Hashes and completely verifies a writer-produced candidate, then mints the
/// only capability accepted by the unchecked-free query construction path.
pub enum CandidatePublicationVerificationError {
    Candidate(IndexError),
    Reusable(ReusablePublicationError),
}

pub fn verify_and_bind_publication_candidate(
    candidate: OpenedPublicationCandidate<'_>,
    topology_authority: Option<&ActiveGenerationPointer>,
    base: Option<&PinnedPublication>,
    base_authority: Option<(&Path, &ActiveGenerationPointer, &GenerationSlot)>,
) -> std::result::Result<VerifiedCandidatePublication, CandidatePublicationVerificationError> {
    let OpenedPublicationCandidate {
        index,
        publication,
        generation_path,
        metas,
    } = candidate;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|error: tantivy::TantivyError| {
            CandidatePublicationVerificationError::Candidate(error.into())
        })?;
    let searcher = reader.searcher();
    if searcher_generation(&searcher) != meta_generation(&metas) {
        return Err(CandidatePublicationVerificationError::Candidate(
            IndexError::ConcurrentGenerationChange,
        ));
    }
    let (generation_id, manifest, publication_metadata) = publication.into_parts();
    let physical_integrity_audit =
        physical_integrity_audit(searcher.index(), generation_path, topology_authority)
            .map_err(|error| CandidatePublicationVerificationError::Candidate(error.into()))?;
    if let Some(base) = base {
        let (root, pointer, slot) = base_authority.ok_or({
            CandidatePublicationVerificationError::Candidate(IndexError::WriterInvariant(
                "incremental candidate verification lacks active base authority",
            ))
        })?;
        verify_pinned_publication_authority(
            root,
            pointer,
            slot,
            base,
            Some(&physical_integrity_audit),
        )
        .map_err(CandidatePublicationVerificationError::Reusable)?;
    }
    verify_publication_candidate(&searcher, &manifest, base.map(PinnedPublication::searcher))
        .map_err(CandidatePublicationVerificationError::Candidate)?;
    Ok(VerifiedCandidatePublication {
        publication: VerifiedPublication {
            searcher,
            manifest: Arc::new(manifest),
            generation_id,
            publication_metadata,
        },
        physical_integrity_audit,
    })
}

/// Distinguishes stale caller binding from confirmed physical-integrity
/// failure so writer recovery never marks concurrency as corruption.
pub enum ReusablePublicationError {
    Binding(IndexError),
    Integrity(IndexError),
}

/// Revalidates that a previously pinned publication still carries the exact
/// pointer-bound physical authority used to mint it. A successful
/// certification fast path reads no artifact bodies; this terminal fence keeps
/// retained-segment exclusions from outliving the immutable base they trust.
pub fn verify_pinned_publication_authority(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    publication: &PinnedPublication,
    candidate_audit: Option<&PhysicalIntegrityAudit>,
) -> std::result::Result<(), ReusablePublicationError> {
    if slot.generation_id() != publication.generation_id {
        return Err(ReusablePublicationError::Binding(
            IndexError::ConcurrentGenerationChange,
        ));
    }
    verify_certified_physical_integrity(
        root,
        pointer,
        slot,
        &publication.physical_integrity,
        candidate_audit,
    )
    .map_err(|error| ReusablePublicationError::Integrity(error.into()))
}

/// Revalidates the durable physical authority for an already-published base
/// and mints a query-reader capability without reopening or re-decoding it.
pub fn verify_and_bind_reusable_publication(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    publication: PinnedPublication,
) -> std::result::Result<VerifiedPublication, ReusablePublicationError> {
    if slot.generation_id() != publication.generation_id {
        return Err(ReusablePublicationError::Binding(
            IndexError::ConcurrentGenerationChange,
        ));
    }
    verify_or_certify_physical_integrity(root, pointer, slot, publication.searcher.index())
        .map_err(|error| ReusablePublicationError::Integrity(error.into()))?;
    Ok(VerifiedPublication {
        searcher: publication.searcher,
        manifest: Arc::new(publication.manifest),
        generation_id: publication.generation_id,
        publication_metadata: publication.publication_metadata,
    })
}

/// Verifies the complete publication authority carried by one immutable searcher.
pub fn verify_complete_searcher(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    expected_physical_integrity_digest: &str,
) -> Result<()> {
    verify_physical_integrity(
        searcher.index(),
        generation_path,
        topology_authority,
        expected_physical_integrity_digest,
    )?;
    verify_searcher(searcher, manifest)
}

const MAX_VERIFICATION_WORKERS: usize = 24;

/// Verifies a writer-produced candidate without replaying an already-audited base.
///
/// A cold, recovery, or all-changed candidate has no reusable candidate segment
/// and therefore keeps the complete stored-Core and posting audit. For a
/// genuinely incremental candidate, every segment not present in the immutable
/// base contributes its event and session identity terms to an identity-delta
/// audit. Every changed Core record is fully decoded once. Each changed identity
/// is then resolved against all live candidate segments, while an already-audited
/// retained identity is decoded at most once per role and term. This preserves
/// duplicate/collision and cross-source session ownership checks without
/// replaying unrelated terms or retained records that share one session.
pub fn verify_publication_candidate(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    base_searcher: Option<&Searcher>,
) -> Result<()> {
    with_verification_scratch_budget(|| {
        verify_publication_candidate_with_budget(searcher, manifest, base_searcher)
    })
}

fn verify_publication_candidate_with_budget(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    base_searcher: Option<&Searcher>,
) -> Result<()> {
    let Some(base_searcher) = base_searcher else {
        return verify_searcher(searcher, manifest);
    };

    let base_segment_ids = base_searcher
        .segment_readers()
        .iter()
        .map(|segment| segment.segment_id().uuid_string())
        .collect::<HashSet<_>>();
    let candidate_segments = searcher.segment_readers();
    let changed_segments = candidate_segments
        .iter()
        .enumerate()
        .filter_map(|(segment_ord, segment)| {
            (!base_segment_ids.contains(&segment.segment_id().uuid_string())).then_some(segment_ord)
        })
        .collect::<Vec<_>>();
    if changed_segments.len() == candidate_segments.len() {
        return verify_searcher(searcher, manifest);
    }

    verify_searcher_structure(searcher, manifest)?;
    let fields = fields_from_schema(searcher.schema())?;
    validate_verification_projection(fields)?;
    let mut changed_identities = IdentityDeltaSpill::create()?;
    let expected_parent_sessions = verify_candidate_event_identities(
        searcher,
        fields,
        &changed_segments,
        &mut changed_identities,
    )?;
    verify_candidate_session_identities(
        searcher,
        fields,
        &changed_segments,
        expected_parent_sessions,
    )?;
    verify_incremental_lineage(
        searcher,
        base_searcher,
        fields,
        &changed_segments,
        &changed_identities,
    )
}

fn verify_candidate_event_identities(
    searcher: &Searcher,
    fields: crate::Fields,
    changed_segments: &[usize],
    changed_identities: &mut IdentityDeltaSpill,
) -> Result<u64> {
    if changed_segments.is_empty() {
        return Ok(0);
    }
    let segments = searcher.segment_readers();
    let changed_segment_set = changed_segments.iter().copied().collect::<HashSet<_>>();
    let expected_changed_documents =
        changed_segments.iter().try_fold(0_u64, |total, &ordinal| {
            total
                .checked_add(u64::from(segments[ordinal].num_docs()))
                .ok_or(IndexError::CountOverflow)
        })?;
    let changed_inverted = changed_segments
        .iter()
        .map(|segment_ord| segments[*segment_ord].inverted_index(fields.event_id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = changed_inverted
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut changed_documents = 0_u64;
    let mut parent_sessions = 0_u64;
    let mut projection_verifier = IncrementalProjectionVerifier::new(searcher, changed_segments)?;
    while merged.advance() {
        note_candidate_identity_term();
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let mut digest = None;
        for (segment_ord, segment) in segments.iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_id)?;
            let Some(term_info) = inverted.terms().get(merged.key())? else {
                continue;
            };
            for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
                note_candidate_identity_document();
                let identities = if changed_segment_set.contains(&segment_ord) {
                    let record = stored_verification_record(searcher, address, fields)?;
                    let identities = record.identities;
                    projection_verifier.verify_document(searcher, fields, address, record)?;
                    changed_documents = changed_documents
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    parent_sessions = parent_sessions
                        .checked_add(u64::from(identities.parent_session.is_some()))
                        .ok_or(IndexError::CountOverflow)?;
                    changed_identities.push(SpillVerificationIdentities {
                        event: identities.event,
                        session: identities.session,
                        parent_session: identities.parent_session,
                        root_session: identities.root_session,
                        session_relationship: identities.session_relationship,
                        event_origin: identities.event_origin,
                        session_source_ordinal: 0,
                    })?;
                    note_candidate_lineage_spill();
                    identities
                } else {
                    stored_verification_identities(searcher, address, fields)?
                };
                let identity = identities.event;
                if identity.as_uuid() != uuid {
                    return Err(IndexError::InvalidStoredDocumentField("event_id"));
                }
                match digest {
                    None => digest = Some(identity.digest),
                    Some(existing) if existing == identity.digest => {
                        return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
                    }
                    Some(existing) => {
                        return Err(IndexError::CompactIdentityCollision {
                            kind: "event",
                            uuid,
                            existing_digest: hex(&existing),
                            new_digest: hex(&identity.digest),
                        });
                    }
                }
                Ok(())
            })?;
        }
    }
    if changed_documents != expected_changed_documents {
        return Err(IndexError::InvalidStoredDocumentField("event_id"));
    }
    projection_verifier.finish(searcher, fields, changed_segments)?;
    Ok(parent_sessions)
}

fn verify_candidate_session_identities(
    searcher: &Searcher,
    fields: crate::Fields,
    changed_segments: &[usize],
    expected_parent_sessions: u64,
) -> Result<()> {
    if changed_segments.is_empty() {
        return Ok(());
    }
    let segments = searcher.segment_readers();
    let changed_segment_set = changed_segments.iter().copied().collect::<HashSet<_>>();
    let expected_changed_documents =
        changed_segments.iter().try_fold(0_u64, |total, &ordinal| {
            total
                .checked_add(u64::from(segments[ordinal].num_docs()))
                .ok_or(IndexError::CountOverflow)
        })?;
    let roles = [
        (fields.session_id, IdentityFieldRole::Session),
        (fields.parent_session_id, IdentityFieldRole::ParentSession),
        (fields.root_session_id, IdentityFieldRole::RootSession),
    ];
    let changed_inverted = roles
        .iter()
        .flat_map(|(field, _)| {
            changed_segments
                .iter()
                .map(move |segment_ord| segments[*segment_ord].inverted_index(*field))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = changed_inverted
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut changed_occurrences = [0_u64; 3];
    while merged.advance() {
        note_candidate_identity_term();
        let uuid = canonical_uuid_term(merged.key(), "session_id")?;
        let mut digest = None;
        let mut owner = None::<[u8; 32]>;
        for (field, role) in roles {
            let mut decoded_retained_identity = false;
            for (segment_ord, segment) in segments.iter().enumerate() {
                let inverted = segment.inverted_index(field)?;
                let Some(term_info) = inverted.terms().get(merged.key())? else {
                    continue;
                };
                for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
                    let changed = changed_segment_set.contains(&segment_ord);
                    if changed {
                        let role_index = match role {
                            IdentityFieldRole::Session => 0,
                            IdentityFieldRole::ParentSession => 1,
                            IdentityFieldRole::RootSession => 2,
                        };
                        changed_occurrences[role_index] = changed_occurrences[role_index]
                            .checked_add(1)
                            .ok_or(IndexError::CountOverflow)?;
                    } else if std::mem::replace(&mut decoded_retained_identity, true) {
                        return Ok(());
                    }
                    note_candidate_identity_document();
                    let identities = stored_verification_identities(searcher, address, fields)?;
                    let (identity, candidate_owner) = match role {
                        IdentityFieldRole::Session => {
                            (identities.session, Some(identities.session_source_owner))
                        }
                        IdentityFieldRole::ParentSession => (
                            identities.parent_session.ok_or(
                                IndexError::InvalidStoredDocumentField("parent_session_id"),
                            )?,
                            None,
                        ),
                        IdentityFieldRole::RootSession => (identities.root_session, None),
                    };
                    if identity.as_uuid() != uuid {
                        return Err(IndexError::InvalidStoredDocumentField("session_id"));
                    }
                    match digest {
                        None => digest = Some(identity.digest),
                        Some(existing) if existing == identity.digest => {}
                        Some(existing) => {
                            return Err(IndexError::CompactIdentityCollision {
                                kind: "session",
                                uuid,
                                existing_digest: hex(&existing),
                                new_digest: hex(&identity.digest),
                            });
                        }
                    }
                    if let Some(candidate_owner) = candidate_owner {
                        match owner {
                            Some(existing) if existing != candidate_owner => {
                                return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
                            }
                            None => owner = Some(candidate_owner),
                            _ => {}
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
    if changed_occurrences
        != [
            expected_changed_documents,
            expected_parent_sessions,
            expected_changed_documents,
        ]
    {
        return Err(IndexError::InvalidStoredDocumentField("session_id"));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
fn note_candidate_identity_term() {
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
fn note_candidate_identity_term() {}

#[cfg(any(test, feature = "test-support"))]
fn note_candidate_identity_document() {
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
fn note_candidate_identity_document() {}

#[cfg(any(test, feature = "test-support"))]
fn note_candidate_projection_document() {
    CANDIDATE_PROJECTION_DOCUMENTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
fn note_candidate_projection_document() {}

#[cfg(any(test, feature = "test-support"))]
pub fn note_candidate_lineage_decode() {
    CANDIDATE_LINEAGE_DECODES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
pub fn note_candidate_lineage_decode() {}

#[cfg(any(test, feature = "test-support"))]
pub fn note_candidate_lineage_spill() {
    CANDIDATE_LINEAGE_SPILLS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
pub fn note_candidate_lineage_spill() {}

fn verification_worker_budget(document_count: u64) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    usize::try_from(document_count)
        .unwrap_or(usize::MAX)
        .max(1)
        .min(available)
        .min(MAX_VERIFICATION_WORKERS)
}

#[cfg(test)]
#[test]
fn verification_tasks_split_large_segments_into_contiguous_bounded_ranges() {
    let max_docs = [1_052_077, 976_361, 131_836, 3_341];
    let tasks = segment_verification_tasks_for_max_docs(&max_docs, 24).unwrap();
    assert!(tasks.len() > 24);

    for (segment_ord, max_doc) in max_docs.into_iter().enumerate() {
        let segment_tasks = tasks
            .iter()
            .filter(|task| task.segment_ord == segment_ord)
            .collect::<Vec<_>>();
        assert_eq!(segment_tasks.first().unwrap().start_doc_id, 0);
        assert_eq!(segment_tasks.last().unwrap().end_doc_id, max_doc);
        assert!(segment_tasks
            .windows(2)
            .all(|pair| pair[0].end_doc_id == pair[1].start_doc_id));
    }
}

include!("verification/logical.rs");

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct VerificationMetrics {
    pub worker_budget: usize,
    pub segment_tasks: usize,
    pub document_decodes: usize,
    pub source_terms: usize,
    pub max_active_workers: usize,
    pub max_buffered_segments: usize,
    pub max_buffered_event_identities: usize,
    pub max_buffered_session_identities: usize,
    pub stored_core_bytes: u64,
    pub body_tokens: u64,
    pub verification_spill_bytes: u64,
    pub verification_tracked_heap_bytes: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub fn verify_searcher_with_metrics(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    worker_budget: usize,
    synchronize_first_wave: bool,
) -> Result<VerificationMetrics> {
    let metrics = verify_searcher_with_options(
        searcher,
        manifest,
        worker_budget,
        true,
        synchronize_first_wave,
    )?;
    Ok(VerificationMetrics {
        worker_budget: metrics.worker_budget,
        segment_tasks: metrics.segment_tasks,
        document_decodes: metrics.document_decodes,
        source_terms: metrics.source_terms,
        max_active_workers: metrics.max_active_workers,
        max_buffered_segments: metrics.max_buffered_segments,
        max_buffered_event_identities: metrics.max_buffered_event_identities,
        max_buffered_session_identities: metrics.max_buffered_session_identities,
        stored_core_bytes: metrics.stored_core_bytes,
        body_tokens: metrics.body_tokens,
        verification_spill_bytes: metrics.verification_spill_bytes,
        verification_tracked_heap_bytes: metrics.verification_tracked_heap_bytes,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_verification_activity() {
    ctx_history_index_generation::reset_physical_verification_activity();
    LOGICAL_PASSES.with(|count| count.set(0));
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(0));
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_PROJECTION_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_LINEAGE_DECODES.with(|count| count.set(0));
    CANDIDATE_LINEAGE_SPILLS.with(|count| count.set(0));
    COMPLETE_SESSION_ID_TRAVERSALS.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn verification_activity() -> (usize, usize) {
    (
        ctx_history_index_generation::checksum_walks(),
        LOGICAL_PASSES.with(Cell::get),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_identity_verification_activity() -> (usize, usize) {
    (
        CANDIDATE_IDENTITY_TERMS.with(Cell::get),
        CANDIDATE_IDENTITY_DOCUMENTS.with(Cell::get),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_projection_verification_activity() -> usize {
    CANDIDATE_PROJECTION_DOCUMENTS.with(Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_lineage_verification_activity() -> (usize, usize) {
    (
        CANDIDATE_LINEAGE_DECODES.with(Cell::get),
        CANDIDATE_LINEAGE_SPILLS.with(Cell::get),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn complete_session_id_traversals() -> usize {
    COMPLETE_SESSION_ID_TRAVERSALS.with(Cell::get)
}
