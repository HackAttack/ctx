use super::*;

pub(super) fn verify_event_identities(
    searcher: &Searcher,
    field: Field,
    expected: u64,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    let segments = searcher.segment_readers();
    let inverted_indexes = segments
        .iter()
        .map(|segment| segment.inverted_index(field))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = 0_u64;
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let projection_digest = query_projection_digest(field, merged.key());
        let mut seen = false;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            for_each_live_posting(
                &inverted_indexes[segment_ord],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences = occurrences
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    projection_deltas.accumulate(address, &projection_digest)?;
                    if std::mem::replace(&mut seen, true) {
                        return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
                    }
                    Ok(())
                },
            )?;
        }
    }
    if occurrences != expected {
        return Err(IndexError::InvalidStoredDocumentField("event_id"));
    }
    Ok(())
}

pub(super) fn verify_session_identities(
    searcher: &Searcher,
    fields: [(Field, IdentityFieldRole); 3],
    expected_occurrences: [u64; 3],
    verification_spill: &VerificationSpill,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    COMPLETE_SESSION_ID_TRAVERSALS.with(|count| count.set(count.get().saturating_add(1)));
    let segments = searcher.segment_readers();
    let mut mappings = Vec::with_capacity(fields.len() * segments.len());
    let mut inverted_indexes = Vec::with_capacity(fields.len() * segments.len());
    for (role_index, (field, role)) in fields.into_iter().enumerate() {
        for (segment_ord, segment) in segments.iter().enumerate() {
            inverted_indexes.push(segment.inverted_index(field)?);
            mappings.push((segment_ord, role_index, role, field));
        }
    }
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = [0_u64; 3];
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "session_id")?;
        let mut digest = None;
        let mut owner = None::<u32>;
        for (stream_index, term_info) in merged.current_segment_ords_and_term_infos() {
            let (segment_ord, role_index, role, field) = mappings[stream_index];
            let projection_digest = query_projection_digest(field, merged.key());
            for_each_live_posting(
                &inverted_indexes[stream_index],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences[role_index] = occurrences[role_index]
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    projection_deltas.accumulate(address, &projection_digest)?;
                    let (identity, source_owner) =
                        identity_for_role(verification_spill, address, role)?;
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
                    if let Some(candidate_owner) = source_owner {
                        match owner {
                            Some(existing) if existing != candidate_owner => {
                                return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
                            }
                            None => owner = Some(candidate_owner),
                            _ => {}
                        }
                    }
                    Ok(())
                },
            )?;
        }
    }
    if occurrences != expected_occurrences {
        return Err(IndexError::InvalidStoredDocumentField("session_id"));
    }
    Ok(())
}

fn identity_for_role(
    verification_spill: &VerificationSpill,
    address: DocAddress,
    role: IdentityFieldRole,
) -> Result<(CompactIdentity, Option<u32>)> {
    let identities = verification_spill.record(address, "session_id")?;
    match role {
        IdentityFieldRole::Session => {
            Ok((identities.session, Some(identities.session_source_ordinal)))
        }
        IdentityFieldRole::ParentSession => Ok((
            identities
                .parent_session
                .ok_or(IndexError::InvalidStoredDocumentField("parent_session_id"))?,
            None,
        )),
        IdentityFieldRole::RootSession => Ok((
            identities
                .root_session
                .ok_or(IndexError::InvalidStoredDocumentField("root_session_id"))?,
            None,
        )),
    }
}

pub(super) fn for_each_live_posting(
    inverted: &InvertedIndexReader,
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

pub(super) fn canonical_uuid_term(term: &[u8], field: &'static str) -> Result<Uuid> {
    let term =
        std::str::from_utf8(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    let uuid = Uuid::parse_str(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    if uuid.to_string() != term {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(uuid)
}
