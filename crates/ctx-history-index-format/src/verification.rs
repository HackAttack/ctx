// Explicit scrub and legacy test-support helpers remain available even when
// the production candidate path no longer calls their incremental replay code.
#![allow(dead_code)]

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
    stored_verification_record, validate_schema, validate_verification_projection,
    verify_certified_physical_integrity, verify_or_certify_physical_integrity,
    ActiveGenerationPointer, CandidatePhysicalProof, CertifiedPhysicalIntegrity, CompactIdentity,
    Fields, GenerationManifest, GenerationSlot, IdentityFieldRole, IndexError, LoadedPublication,
    PhysicalIntegrityAudit, Result, VerificationRecord,
};

use super::{physical_integrity_audit_with_candidate_proof, verify_physical_integrity};

mod lineage;
mod spill;

use lineage::SessionRelationship;
use spill::{
    reserve_verification_scratch, with_verification_scratch_budget, ProjectionAccumulator,
    ProjectionDeltas, ScratchReservation, SpillVerificationIdentities, VerificationSpill,
    VERIFICATION_SPILL_BUFFER_BYTES, VERIFICATION_SPILL_RECORD_BYTES,
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
    static CANDIDATE_IDENTITY_TRAVERSALS: Cell<usize> = const { Cell::new(0) };
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
    verify_and_bind_publication_candidate_with_progress(
        candidate,
        topology_authority,
        base,
        base_authority,
        None,
        || Ok(()),
    )
}

/// Verifies a writer candidate while exposing the exact boundary between its
/// physical artifact audit and logical Core verification.
#[doc(hidden)]
pub fn verify_and_bind_publication_candidate_with_progress<P>(
    candidate: OpenedPublicationCandidate<'_>,
    topology_authority: Option<&ActiveGenerationPointer>,
    base: Option<&PinnedPublication>,
    base_authority: Option<(&Path, &ActiveGenerationPointer, &GenerationSlot)>,
    candidate_physical_proof: Option<&CandidatePhysicalProof>,
    report_logical_verification: P,
) -> std::result::Result<VerifiedCandidatePublication, CandidatePublicationVerificationError>
where
    P: FnOnce() -> Result<()>,
{
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
    let physical_integrity_audit = physical_integrity_audit_with_candidate_proof(
        searcher.index(),
        generation_path,
        topology_authority,
        candidate_physical_proof,
    )
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
    report_logical_verification().map_err(CandidatePublicationVerificationError::Candidate)?;
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

/// Verifies the compact logical invariants required to publish a writer candidate.
///
/// A cold or all-changed candidate traverses the complete live `event_id` term
/// and posting set once. A genuinely incremental candidate traverses only terms
/// introduced by changed segments, resolving each such term across candidate
/// segments to reject a duplicate retained identity. Retained segments are
/// trusted through the separately revalidated base authority. This path never
/// decodes stored Core or replays query, source, session, or lineage projections;
/// [`verify_searcher`] retains that exhaustive behavior for explicit scrubbing.
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
    verify_searcher_structure(searcher, manifest)?;
    let candidate_segments = searcher.segment_readers();
    let changed_segments = if let Some(base_searcher) = base_searcher {
        let base_segment_ids = base_searcher
            .segment_readers()
            .iter()
            .map(|segment| segment.segment_id().uuid_string())
            .collect::<HashSet<_>>();
        let changed_segments = candidate_segments
            .iter()
            .enumerate()
            .filter_map(|(segment_ord, segment)| {
                (!base_segment_ids.contains(&segment.segment_id().uuid_string()))
                    .then_some(segment_ord)
            })
            .collect::<Vec<_>>();
        if changed_segments.is_empty() {
            return Ok(());
        }
        changed_segments
    } else {
        (0..candidate_segments.len()).collect::<Vec<_>>()
    };
    let scan_all_segments = changed_segments.len() == candidate_segments.len();
    let event_id = crate::required_field(searcher.schema(), "event_id")?;
    verify_candidate_event_identities(searcher, event_id, &changed_segments, scan_all_segments)
}

fn verify_candidate_event_identities(
    searcher: &Searcher,
    event_id: Field,
    changed_segments: &[usize],
    scan_all_segments: bool,
) -> Result<()> {
    note_candidate_identity_traversal();
    let segments = searcher.segment_readers();
    let mut visits = CandidateIdentityVisits::new(segments, changed_segments)?;
    let inverted_indexes = segments
        .iter()
        .map(|segment| segment.inverted_index(event_id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = changed_segments
        .iter()
        .map(|segment_ord| inverted_indexes[*segment_ord].terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    while merged.advance() {
        note_candidate_identity_term();
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let mut seen = false;
        if scan_all_segments {
            for (stream_ord, term_info) in merged.current_segment_ords_and_term_infos() {
                let segment_ord = changed_segments[stream_ord];
                verify_candidate_event_postings(
                    &inverted_indexes[segment_ord],
                    &term_info,
                    segment_ord,
                    &segments[segment_ord],
                    uuid,
                    &mut seen,
                    &mut visits,
                )?;
            }
        } else {
            for (segment_ord, segment) in segments.iter().enumerate() {
                let Some(term_info) = inverted_indexes[segment_ord].terms().get(merged.key())?
                else {
                    continue;
                };
                verify_candidate_event_postings(
                    &inverted_indexes[segment_ord],
                    &term_info,
                    segment_ord,
                    segment,
                    uuid,
                    &mut seen,
                    &mut visits,
                )?;
            }
        }
    }
    visits.finish()
}

fn verify_candidate_event_postings(
    inverted: &InvertedIndexReader,
    term_info: &tantivy::postings::TermInfo,
    segment_ord: usize,
    segment: &tantivy::SegmentReader,
    uuid: Uuid,
    seen: &mut bool,
    visits: &mut CandidateIdentityVisits,
) -> Result<()> {
    for_each_live_posting(inverted, term_info, segment_ord, segment, |address| {
        note_candidate_identity_document();
        visits.note(address)?;
        if std::mem::replace(seen, true) {
            return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
        }
        Ok(())
    })
}

struct CandidateIdentityVisits {
    segments: Vec<Option<SegmentIdentityVisits>>,
    _reservation: ScratchReservation,
}

struct SegmentIdentityVisits {
    expected: u64,
    seen: u64,
    words: Vec<u64>,
}

impl CandidateIdentityVisits {
    fn new(segments: &[tantivy::SegmentReader], audited_segments: &[usize]) -> Result<Self> {
        let heap_bytes = audited_segments
            .iter()
            .try_fold(0_u64, |total, &segment_ord| {
                let words = u64::from(segments[segment_ord].max_doc()).div_ceil(64);
                total
                    .checked_add(words.checked_mul(8).ok_or(IndexError::CountOverflow)?)
                    .ok_or(IndexError::CountOverflow)
            })?;
        let reservation = reserve_verification_scratch(0, heap_bytes)?;
        let mut visits = (0..segments.len()).map(|_| None).collect::<Vec<_>>();
        for &segment_ord in audited_segments {
            let segment = &segments[segment_ord];
            let word_count = usize::try_from(u64::from(segment.max_doc()).div_ceil(64))
                .map_err(|_| IndexError::CountOverflow)?;
            visits[segment_ord] = Some(SegmentIdentityVisits {
                expected: u64::from(segment.num_docs()),
                seen: 0,
                words: vec![0; word_count],
            });
        }
        Ok(Self {
            segments: visits,
            _reservation: reservation,
        })
    }

    fn note(&mut self, address: DocAddress) -> Result<()> {
        let segment_ord =
            usize::try_from(address.segment_ord).map_err(|_| IndexError::CountOverflow)?;
        let Some(segment) = self.segments.get_mut(segment_ord).and_then(Option::as_mut) else {
            return Ok(());
        };
        let word = usize::try_from(address.doc_id / 64).map_err(|_| IndexError::CountOverflow)?;
        let mask = 1_u64 << (address.doc_id % 64);
        let word = segment
            .words
            .get_mut(word)
            .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
        if *word & mask != 0 {
            return Err(IndexError::InvalidStoredDocumentField("event_id"));
        }
        *word |= mask;
        segment.seen = segment
            .seen
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn finish(self) -> Result<()> {
        if self
            .segments
            .iter()
            .flatten()
            .any(|segment| segment.seen != segment.expected)
        {
            return Err(IndexError::InvalidStoredDocumentField("event_id"));
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
fn note_candidate_identity_traversal() {
    CANDIDATE_IDENTITY_TRAVERSALS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, feature = "test-support")))]
fn note_candidate_identity_traversal() {}

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

#[cfg(test)]
mod candidate_identity_tests {
    use tantivy::{indexer::NoMergePolicy, TantivyDocument};

    use super::*;

    fn manifest(document_count: usize) -> GenerationManifest {
        let mut manifest = GenerationManifest::from_sources(Vec::new()).unwrap();
        manifest.indexed_documents = u64::try_from(document_count).unwrap();
        manifest
    }

    fn document(event_ids: impl IntoIterator<Item = Uuid>, event_id: Field) -> TantivyDocument {
        let mut document = TantivyDocument::default();
        for identity in event_ids {
            document.add_text(event_id, identity.to_string());
        }
        document
    }

    fn cold_searcher(documents: Vec<Vec<Uuid>>) -> Searcher {
        let schema = crate::lexical_schema();
        let event_id = crate::required_field(&schema, "event_id").unwrap();
        let index = Index::create_in_ram(schema);
        crate::register_body_analyzer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        for event_ids in documents {
            writer.add_document(document(event_ids, event_id)).unwrap();
        }
        writer.commit().unwrap();
        index.reader().unwrap().searcher()
    }

    fn incremental_searchers(retained: usize, appended: Uuid) -> (Searcher, Searcher) {
        let schema = crate::lexical_schema();
        let event_id = crate::required_field(&schema, "event_id").unwrap();
        let index = Index::create_in_ram(schema);
        crate::register_body_analyzer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        for value in 1..=retained {
            writer
                .add_document(document([Uuid::from_u128(value as u128)], event_id))
                .unwrap();
        }
        writer.commit().unwrap();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let base = reader.searcher();
        writer.add_document(document([appended], event_id)).unwrap();
        writer.commit().unwrap();
        reader.reload().unwrap();
        (base, reader.searcher())
    }

    fn assert_compact_candidate_activity(traversals: usize, terms: usize, postings: usize) {
        assert_eq!(verification_activity().1, 0, "exhaustive logical pass");
        assert_eq!(candidate_identity_traversals(), traversals);
        assert_eq!(
            candidate_identity_verification_activity(),
            (terms, postings)
        );
        assert_eq!(candidate_projection_verification_activity(), 0);
        assert_eq!(candidate_lineage_verification_activity(), (0, 0));
    }

    #[test]
    fn cold_candidate_performs_one_compact_identity_traversal_without_core_decodes() {
        let documents = (1..=32)
            .map(|value| vec![Uuid::from_u128(value)])
            .collect::<Vec<_>>();
        let searcher = cold_searcher(documents);

        reset_verification_activity();
        verify_publication_candidate(&searcher, &manifest(32), None).unwrap();

        assert_compact_candidate_activity(1, 32, 32);
    }

    #[test]
    fn one_record_append_identity_work_is_independent_of_retained_document_count() {
        fn activity(retained: usize) -> (usize, (usize, usize)) {
            let appended = Uuid::from_u128(retained as u128 + 1);
            let (base, candidate) = incremental_searchers(retained, appended);
            reset_verification_activity();
            verify_publication_candidate(&candidate, &manifest(retained + 1), Some(&base)).unwrap();
            assert_compact_candidate_activity(1, 1, 1);
            (
                candidate_identity_traversals(),
                candidate_identity_verification_activity(),
            )
        }

        assert_eq!(activity(64), activity(128));
    }

    #[test]
    fn unchanged_segment_set_performs_zero_identity_work() {
        let searcher = cold_searcher(vec![vec![Uuid::from_u128(1)]]);

        reset_verification_activity();
        verify_publication_candidate(&searcher, &manifest(1), Some(&searcher)).unwrap();

        assert_compact_candidate_activity(0, 0, 0);
    }

    #[test]
    fn append_rejects_an_event_id_already_present_in_the_retained_base() {
        let duplicate = Uuid::from_u128(1);
        let (base, candidate) = incremental_searchers(16, duplicate);

        let error =
            verify_publication_candidate(&candidate, &manifest(17), Some(&base)).unwrap_err();

        assert!(
            matches!(error, IndexError::DuplicateEventIdentity(value) if value == duplicate.to_string())
        );
    }

    #[test]
    fn cold_candidate_rejects_missing_or_extra_event_id_occurrences() {
        let missing = cold_searcher(vec![Vec::new()]);
        assert!(matches!(
            verify_publication_candidate(&missing, &manifest(1), None),
            Err(IndexError::InvalidStoredDocumentField("event_id"))
        ));

        let extra = cold_searcher(vec![vec![Uuid::from_u128(1), Uuid::from_u128(2)]]);
        assert!(matches!(
            verify_publication_candidate(&extra, &manifest(1), None),
            Err(IndexError::InvalidStoredDocumentField("event_id"))
        ));
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
    CANDIDATE_IDENTITY_TRAVERSALS.with(|count| count.set(0));
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(0));
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_PROJECTION_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_LINEAGE_DECODES.with(|count| count.set(0));
    CANDIDATE_LINEAGE_SPILLS.with(|count| count.set(0));
    COMPLETE_SESSION_ID_TRAVERSALS.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn candidate_identity_traversals() -> usize {
    CANDIDATE_IDENTITY_TRAVERSALS.with(Cell::get)
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
