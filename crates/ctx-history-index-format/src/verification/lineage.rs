use std::collections::{HashMap, HashSet};

use ctx_history_core::SessionRelationshipKind;
use tantivy::{schema::IndexRecordOption, DocAddress, DocSet, Searcher, Term, TERMINATED};

use crate::{stored_verification_identities, CompactIdentity, Fields, IndexError, Result};

use super::{
    note_candidate_lineage_decode, note_candidate_lineage_spill,
    spill::{IdentityDeltaSpill, IdentityKeySpill, SpillVerificationIdentities, VerificationSpill},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionRelationship {
    parent: Option<CompactIdentity>,
    claimed_root: CompactIdentity,
    kind: SessionRelationshipKind,
}

/// Verifies only the changed sessions' own durable claims.
///
/// Parent, claimed-root, and copied-event targets deliberately remain outside
/// publication authority. They may be absent or cyclic and are resolved only
/// by an explicit lineage query.
pub(super) fn verify_incremental_lineage(
    searcher: &Searcher,
    base_searcher: &Searcher,
    fields: Fields,
    changed_segments: &[usize],
    changed: &IdentityDeltaSpill,
) -> Result<()> {
    let changed_segments = changed_segments.iter().copied().collect::<HashSet<_>>();
    let retired = retired_identity_delta(searcher, base_searcher, fields)?;
    let mut affected_sessions = IdentityKeySpill::create()?;
    changed.for_each(|identities| {
        affected_sessions.push(identities.session)?;
        note_candidate_lineage_spill();
        Ok(())
    })?;
    retired.for_each(|identities| {
        affected_sessions.push(identities.session)?;
        note_candidate_lineage_spill();
        Ok(())
    })?;

    affected_sessions.for_each_unique(|session| {
        resolve_session_indexed(searcher, fields, session, Some(&changed_segments)).map(|_| ())
    })
}

fn retired_identity_delta(
    searcher: &Searcher,
    base_searcher: &Searcher,
    fields: Fields,
) -> Result<IdentityDeltaSpill> {
    let candidate_segments = searcher.segment_readers();
    let candidate_by_id = candidate_segments
        .iter()
        .enumerate()
        .map(|(ordinal, segment)| (segment.segment_id().uuid_string(), ordinal))
        .collect::<HashMap<_, _>>();
    let mut retired = IdentityDeltaSpill::create()?;
    for (base_ordinal, base_segment) in base_searcher.segment_readers().iter().enumerate() {
        let candidate = candidate_by_id
            .get(&base_segment.segment_id().uuid_string())
            .map(|ordinal| &candidate_segments[*ordinal]);
        if candidate
            .is_some_and(|segment| segment.num_deleted_docs() == base_segment.num_deleted_docs())
        {
            continue;
        }
        for doc_id in 0..base_segment.max_doc() {
            if base_segment.is_deleted(doc_id)
                || candidate.is_some_and(|segment| !segment.is_deleted(doc_id))
            {
                continue;
            }
            retired.push(indexed_identities(
                base_searcher,
                DocAddress::new(
                    u32::try_from(base_ordinal).map_err(|_| IndexError::CountOverflow)?,
                    doc_id,
                ),
                fields,
            )?)?;
            note_candidate_lineage_spill();
        }
    }
    Ok(retired)
}

fn indexed_identities(
    searcher: &Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<SpillVerificationIdentities> {
    note_candidate_lineage_decode();
    let identities = stored_verification_identities(searcher, address, fields)?;
    Ok(SpillVerificationIdentities {
        event: identities.event,
        session: identities.session,
        parent_session: identities.parent_session,
        root_session: identities.root_session,
        session_relationship: identities.session_relationship,
        event_origin: identities.event_origin,
        session_source_ordinal: 0,
    })
}

fn resolve_session_indexed(
    searcher: &Searcher,
    fields: Fields,
    session: CompactIdentity,
    changed_segments: Option<&HashSet<usize>>,
) -> Result<Option<SessionRelationship>> {
    let term = Term::from_field_text(fields.session_id, &session.as_uuid().to_string());
    let mut resolved = None;
    let mut decoded_retained = false;
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(fields.session_id)?;
        let Some(term_info) = inverted.get_term_info(&term)? else {
            continue;
        };
        for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
            let changed = changed_segments.is_some_and(|segments| segments.contains(&segment_ord));
            if !changed && std::mem::replace(&mut decoded_retained, true) {
                return Ok(());
            }
            let identities = indexed_identities(searcher, address, fields)?;
            if identities.session != session {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "compact session identity collision",
                ));
            }
            let candidate = relationship_for(identities);
            match resolved {
                None => resolved = Some(candidate),
                Some(existing) if existing == candidate => {}
                Some(_) => {
                    return Err(IndexError::InvalidSessionRelationshipGraph(
                        "one session has contradictory relationship fields",
                    ));
                }
            }
            Ok(())
        })?;
    }
    Ok(resolved)
}

/// Verifies local shape and one consistent set of child-owned claims per
/// session. It intentionally does not walk parent, claimed-root, or copy edges.
pub(super) fn verify_lineage(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
) -> Result<()> {
    let segments = searcher.segment_readers();
    let inverted = segments
        .iter()
        .map(|segment| segment.inverted_index(fields.session_id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = inverted
        .iter()
        .map(|index| index.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = tantivy::termdict::TermMerger::new(streams);
    while merged.advance() {
        let mut session = None;
        let mut relationship = None;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            for_each_live_posting(
                &inverted[segment_ord],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    let identities = spill.record(address, "session_relationship")?;
                    match session {
                        None => session = Some(identities.session),
                        Some(existing) if existing == identities.session => {}
                        Some(_) => {
                            return Err(IndexError::InvalidSessionRelationshipGraph(
                                "compact session identity collision",
                            ));
                        }
                    }
                    let candidate = relationship_for(identities);
                    match relationship {
                        None => relationship = Some(candidate),
                        Some(existing) if existing == candidate => {}
                        Some(_) => {
                            return Err(IndexError::InvalidSessionRelationshipGraph(
                                "one session has contradictory relationship fields",
                            ));
                        }
                    }
                    Ok(())
                },
            )?;
        }
    }
    Ok(())
}

fn relationship_for(identities: SpillVerificationIdentities) -> SessionRelationship {
    SessionRelationship {
        parent: identities.parent_session,
        claimed_root: identities.root_session,
        kind: identities.session_relationship,
    }
}

fn for_each_live_posting(
    inverted: &tantivy::InvertedIndexReader,
    term_info: &tantivy::postings::TermInfo,
    segment_ord: usize,
    segment: &tantivy::SegmentReader,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    let mut postings = inverted.read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?;
    let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
    let mut doc_id = postings.doc();
    while doc_id != TERMINATED {
        if !segment.is_deleted(doc_id) {
            visit(DocAddress::new(segment_ord, doc_id))?;
        }
        doc_id = postings.advance();
    }
    Ok(())
}
