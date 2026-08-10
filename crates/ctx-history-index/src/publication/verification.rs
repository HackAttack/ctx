use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use sha2::{Digest, Sha256};
use tantivy::{
    postings::Postings,
    schema::{Field, IndexRecordOption},
    termdict::TermMerger,
    tokenizer::TokenStream,
    DocAddress, DocSet, Executor, InvertedIndexReader, Searcher, Term, TERMINATED,
};
use uuid::Uuid;

#[cfg(test)]
use std::cell::Cell;

use crate::{
    fields_from_schema, hex,
    query::{self, CompactIdentity, IdentityFieldRole},
    staging::{accumulate_core_record, core_record_accumulator_leaf},
    GenerationManifest, IndexError, Result,
};

use super::{verify_physical_integrity, ActiveGenerationPointer};

mod lineage;
mod spill;

use lineage::{verify_incremental_lineage, verify_lineage};
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
    #[cfg(test)]
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

#[cfg(test)]
thread_local! {
    static LOGICAL_PASSES: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_TERMS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_DOCUMENTS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_PROJECTION_DOCUMENTS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_LINEAGE_DECODES: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_LINEAGE_SPILLS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn verify_searcher_structure(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<()> {
    let actual = searcher.num_docs();
    if actual != manifest.indexed_documents {
        return Err(IndexError::DocumentCountMismatch {
            manifest: manifest.indexed_documents,
            index: actual,
        });
    }
    Ok(())
}

pub(crate) fn verify_searcher(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    let worker_budget = verification_worker_budget(searcher.num_docs());
    verify_searcher_with_options(searcher, manifest, worker_budget, false, false).map(|_| ())
}

/// Verifies the complete publication authority carried by one immutable searcher.
pub(crate) fn verify_complete_searcher(
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
pub(crate) fn verify_publication_candidate(
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
    query::validate_verification_projection(fields)?;
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
                    let record = query::stored_verification_record(searcher, address, fields)?;
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
                    query::stored_verification_identities(searcher, address, fields)?
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
                    let identities =
                        query::stored_verification_identities(searcher, address, fields)?;
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

#[cfg(test)]
fn note_candidate_identity_term() {
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_candidate_identity_term() {}

#[cfg(test)]
fn note_candidate_identity_document() {
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_candidate_identity_document() {}

#[cfg(test)]
fn note_candidate_projection_document() {
    CANDIDATE_PROJECTION_DOCUMENTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_candidate_projection_document() {}

#[cfg(test)]
pub(super) fn note_candidate_lineage_decode() {
    CANDIDATE_LINEAGE_DECODES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
pub(super) fn note_candidate_lineage_decode() {}

#[cfg(test)]
pub(super) fn note_candidate_lineage_spill() {
    CANDIDATE_LINEAGE_SPILLS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
pub(super) fn note_candidate_lineage_spill() {}

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

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct VerificationMetrics {
    pub(crate) worker_budget: usize,
    pub(crate) segment_tasks: usize,
    pub(crate) document_decodes: usize,
    pub(crate) source_terms: usize,
    pub(crate) max_active_workers: usize,
    pub(crate) max_buffered_segments: usize,
    pub(crate) max_buffered_event_identities: usize,
    pub(crate) max_buffered_session_identities: usize,
    pub(crate) stored_core_bytes: u64,
    pub(crate) body_tokens: u64,
    pub(crate) verification_spill_bytes: u64,
    pub(crate) verification_tracked_heap_bytes: usize,
}

#[cfg(test)]
pub(crate) fn verify_searcher_with_metrics(
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

#[cfg(test)]
pub(crate) fn reset_verification_activity() {
    ctx_history_index_generation::reset_physical_verification_activity();
    LOGICAL_PASSES.with(|count| count.set(0));
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(0));
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_PROJECTION_DOCUMENTS.with(|count| count.set(0));
    CANDIDATE_LINEAGE_DECODES.with(|count| count.set(0));
    CANDIDATE_LINEAGE_SPILLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn verification_activity() -> (usize, usize) {
    (
        ctx_history_index_generation::checksum_walks(),
        LOGICAL_PASSES.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn candidate_identity_verification_activity() -> (usize, usize) {
    (
        CANDIDATE_IDENTITY_TERMS.with(Cell::get),
        CANDIDATE_IDENTITY_DOCUMENTS.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn candidate_projection_verification_activity() -> usize {
    CANDIDATE_PROJECTION_DOCUMENTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn candidate_lineage_verification_activity() -> (usize, usize) {
    (
        CANDIDATE_LINEAGE_DECODES.with(Cell::get),
        CANDIDATE_LINEAGE_SPILLS.with(Cell::get),
    )
}
