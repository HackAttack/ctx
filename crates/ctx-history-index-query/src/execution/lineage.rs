use ctx_history_core::{
    ProviderNativeCopyProof, ProviderNativeEventCopy, ProviderNativeSessionRelationship, SourceKey,
    StableEntityId, StableEntityKind,
};
use serde::Deserialize;
use tantivy::{
    schema::IndexRecordOption, DocAddress, DocId, DocSet, SegmentReader, TantivyDocument, Term,
    TERMINATED,
};
use uuid::Uuid;

use crate::{fields_from_schema, hex, Fields, IndexError, Result, VerifiedIndex};

use super::super::{
    CopiedEventLineage, CopiedEventLineageOccurrence, CopiedEventLineagePolicy,
    CopiedEventLineageRelationshipCount, CopiedEventLineageResolution,
    MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS,
};

#[derive(Debug, Clone)]
struct LineageEvent {
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    claimed_root_session_id: Option<StableEntityId>,
    session_relationship: Option<ProviderNativeSessionRelationship>,
    event_copy: Option<ProviderNativeEventCopy>,
    event_sequence: u64,
}

impl LineageEvent {
    fn order_key(&self) -> (Uuid, u64, Uuid) {
        (
            self.session_id.as_uuid(),
            self.event_sequence,
            self.event_id.as_uuid(),
        )
    }

    fn target(&self) -> LineageTarget {
        LineageTarget {
            event_id: self.event_id.as_uuid(),
            exact_event_id: Some(self.event_id),
            session_id: Some(self.session_id),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LineageTarget {
    event_id: Uuid,
    exact_event_id: Option<StableEntityId>,
    session_id: Option<StableEntityId>,
}

impl LineageTarget {
    fn requested(event_id: Uuid) -> Self {
        Self {
            event_id,
            exact_event_id: None,
            session_id: None,
        }
    }

    fn claimed(session_id: StableEntityId, event_id: StableEntityId) -> Self {
        Self {
            event_id: event_id.as_uuid(),
            exact_event_id: Some(event_id),
            session_id: Some(session_id),
        }
    }

    fn unresolved(self) -> CopiedEventLineageResolution {
        CopiedEventLineageResolution::Unresolved {
            event_id: self.event_id,
            session_id: self.session_id,
        }
    }
}

struct ForwardResolution {
    selected_session_id: Option<StableEntityId>,
    anchor: LineageTarget,
    resolution: CopiedEventLineageResolution,
    selected_depth: usize,
}

#[derive(Deserialize)]
struct StoredLineageProjection {
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    session_relationship: Option<ProviderNativeSessionRelationship>,
    event_copy: Option<ProviderNativeEventCopy>,
    source: SourceKey,
    event_sequence: u64,
}

impl VerifiedIndex {
    /// Resolves one selected event's direct copied-event target, then returns
    /// exact direct reverse copied-event claims within the caller's explicit
    /// work and retention ceilings.
    ///
    /// Missing selected events and targets are ordinary query results. Parent,
    /// root, and copied-from claims are never treated as transitive graph
    /// authority.
    pub fn copied_event_lineage(
        &self,
        selected_event_id: Uuid,
        policy: CopiedEventLineagePolicy,
    ) -> Result<CopiedEventLineage> {
        policy.validate()?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut exact_identity_posting_visits = 0_usize;
        let selected = self.lineage_event_by_uuid(
            selected_event_id,
            fields,
            &mut exact_identity_posting_visits,
        )?;
        let forward = self.resolve_selected_lineage(
            selected,
            selected_event_id,
            fields,
            &mut exact_identity_posting_visits,
        )?;

        let mut posting_visits = 0_usize;
        let mut relationship_counts = [0_u64; 6];
        let mut occurrences = Vec::with_capacity(policy.maximum_occurrences);
        let mut children = Vec::new();
        let inverse_complete = self.inverse_lineage_children(
            forward.anchor,
            fields,
            policy.maximum_posting_visits,
            &mut posting_visits,
            &mut children,
        )?;
        children.sort_by_key(LineageEvent::order_key);
        let observed_count =
            u64::try_from(children.len()).map_err(|_| IndexError::CountOverflow)?;
        for child in children {
            let count = relationship_counts
                .get_mut(relationship_index(child.session_relationship))
                .ok_or(IndexError::CountOverflow)?;
            *count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;
            let copy = child
                .event_copy
                .as_ref()
                .ok_or(IndexError::InvalidStoredDocumentField(
                    "event_copy_ancestor_event_id",
                ))?;
            if occurrences.len() < policy.maximum_occurrences {
                occurrences.push(CopiedEventLineageOccurrence {
                    event_id: child.event_id,
                    session_id: child.session_id,
                    copied_from_event_id: copy.ancestor_event_id,
                    copied_from_session_id: copy.ancestor_session_id,
                    parent_session_id: child.parent_session_id,
                    claimed_root_session_id: child.claimed_root_session_id,
                    session_relationship: child.session_relationship,
                    copy_proof: copy.proof,
                    depth: 1,
                });
            }
        }

        let relationship_counts = relationship_counts
            .into_iter()
            .zip(RELATIONSHIP_ORDER)
            .filter_map(|(observed_count, session_relationship)| {
                (observed_count != 0).then_some(CopiedEventLineageRelationshipCount {
                    session_relationship,
                    observed_count,
                })
            })
            .collect::<Vec<_>>();
        let returned = occurrences.len();
        Ok(CopiedEventLineage {
            generation_id: self.generation_id.clone(),
            selected_event_id,
            selected_session_id: forward.selected_session_id,
            resolution: forward.resolution,
            selected_depth: forward.selected_depth,
            observed_count,
            returned,
            occurrences,
            relationship_counts,
            truncated: !inverse_complete,
        })
    }

    fn resolve_selected_lineage(
        &self,
        selected: Option<LineageEvent>,
        selected_event_id: Uuid,
        fields: Fields,
        exact_identity_posting_visits: &mut usize,
    ) -> Result<ForwardResolution> {
        let Some(selected) = selected else {
            let anchor = LineageTarget::requested(selected_event_id);
            return Ok(ForwardResolution {
                selected_session_id: None,
                anchor,
                resolution: anchor.unresolved(),
                selected_depth: 0,
            });
        };
        let anchor = selected.target();
        let Some(copy) = selected.event_copy.as_ref() else {
            return Ok(ForwardResolution {
                selected_session_id: Some(selected.session_id),
                anchor,
                resolution: CopiedEventLineageResolution::Resolved {
                    event_id: selected.event_id,
                    session_id: selected.session_id,
                },
                selected_depth: 0,
            });
        };
        let target = LineageTarget::claimed(copy.ancestor_session_id, copy.ancestor_event_id);
        let resolution = match self.lineage_event_by_uuid(
            copy.ancestor_event_id.as_uuid(),
            fields,
            exact_identity_posting_visits,
        )? {
            Some(ancestor)
                if ancestor.event_id == copy.ancestor_event_id
                    && ancestor.session_id == copy.ancestor_session_id =>
            {
                CopiedEventLineageResolution::Resolved {
                    event_id: ancestor.event_id,
                    session_id: ancestor.session_id,
                }
            }
            Some(_) | None => target.unresolved(),
        };
        Ok(ForwardResolution {
            selected_session_id: Some(selected.session_id),
            anchor,
            resolution,
            selected_depth: 1,
        })
    }

    fn lineage_event_by_uuid(
        &self,
        event_id: Uuid,
        fields: Fields,
        exact_identity_posting_visits: &mut usize,
    ) -> Result<Option<LineageEvent>> {
        let term = Term::from_field_text(fields.event_id, &event_id.to_string());
        let mut found = None;
        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_id)?;
            let Some(term_info) = inverted.get_term_info(&term)? else {
                continue;
            };
            let mut postings =
                inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                note_exact_identity_posting(exact_identity_posting_visits)?;
                if !segment.is_deleted(doc_id) {
                    if found.is_some() {
                        return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
                    }
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    let record = stored_lineage_event(
                        &self.searcher,
                        DocAddress::new(segment_ord, doc_id),
                        fields,
                    )?;
                    if record.event_id.as_uuid() != event_id {
                        return Err(IndexError::InvalidStoredDocumentField("event_id"));
                    }
                    found = Some(record);
                }
                doc_id = postings.advance();
            }
        }
        Ok(found)
    }

    fn inverse_lineage_children(
        &self,
        target: LineageTarget,
        fields: Fields,
        maximum_posting_visits: usize,
        posting_visits: &mut usize,
        children: &mut Vec<LineageEvent>,
    ) -> Result<bool> {
        let term = Term::from_field_text(
            fields.event_copy_ancestor_event_id,
            &target.event_id.to_string(),
        );
        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_copy_ancestor_event_id)?;
            let Some(term_info) = inverted.get_term_info(&term)? else {
                continue;
            };
            let mut postings =
                inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if *posting_visits == maximum_posting_visits {
                    return Ok(false);
                }
                *posting_visits = (*posting_visits)
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                if !segment.is_deleted(doc_id) {
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    let child = stored_lineage_event(
                        &self.searcher,
                        DocAddress::new(segment_ord, doc_id),
                        fields,
                    )?;
                    let copy =
                        child
                            .event_copy
                            .as_ref()
                            .ok_or(IndexError::InvalidStoredDocumentField(
                                "event_copy_ancestor_event_id",
                            ))?;
                    if copy.ancestor_event_id.as_uuid() != target.event_id {
                        return Err(IndexError::InvalidStoredDocumentField(
                            "event_copy_ancestor_event_id",
                        ));
                    }
                    if target
                        .exact_event_id
                        .is_some_and(|id| copy.ancestor_event_id != id)
                        || target
                            .session_id
                            .is_some_and(|id| copy.ancestor_session_id != id)
                    {
                        doc_id = postings.advance();
                        continue;
                    }
                    children.push(child);
                }
                doc_id = postings.advance();
            }
        }
        Ok(true)
    }
}

fn note_exact_identity_posting(posting_visits: &mut usize) -> Result<()> {
    if *posting_visits == MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS {
        return Err(
            IndexError::CopiedEventLineageEventAndSessionIdentityPostingVisitLimitExceeded {
                maximum: MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS,
            },
        );
    }
    *posting_visits = (*posting_visits)
        .checked_add(1)
        .ok_or(IndexError::CountOverflow)?;
    Ok(())
}

fn stored_lineage_event(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<LineageEvent> {
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded = ctx_history_index_format::validated_core_record_bytes(
        searcher, address, &document, fields,
    )?;
    let projection: StoredLineageProjection = serde_json::from_slice(encoded)?;
    validate_stored_lineage_projection(&projection)?;
    validate_indexed_lineage_projection(searcher, address, fields, &projection)?;
    Ok(LineageEvent {
        event_id: projection.event_id,
        session_id: projection.session_id,
        parent_session_id: projection.parent_session_id,
        claimed_root_session_id: projection.root_session_id,
        session_relationship: projection.session_relationship,
        event_copy: projection.event_copy.clone(),
        event_sequence: projection.event_sequence,
    })
}

fn validate_stored_lineage_projection(projection: &StoredLineageProjection) -> Result<()> {
    projection.source.validate_contract()?;
    validate_owned_identity(
        projection.event_id,
        StableEntityKind::Event,
        &projection.source,
    )?;
    validate_owned_identity(
        projection.session_id,
        StableEntityKind::Session,
        &projection.source,
    )?;
    if let Some(root_session_id) = projection.root_session_id {
        validate_related_session_identity(root_session_id)?;
    }
    if let Some(parent_session_id) = projection.parent_session_id {
        validate_related_session_identity(parent_session_id)?;
    }
    if let Some(copy) = &projection.event_copy {
        validate_related_session_identity(copy.ancestor_session_id)?;
        copy.ancestor_event_id.validate_contract()?;
        if copy.ancestor_event_id.entity_kind() != StableEntityKind::Event
            || copy.ancestor_session_id == projection.session_id
            || copy.ancestor_event_id == projection.event_id
        {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
    }
    Ok(())
}

fn validate_owned_identity(
    identity: StableEntityId,
    expected_kind: StableEntityKind,
    source: &SourceKey,
) -> Result<()> {
    identity.validate_contract()?;
    if identity.entity_kind() != expected_kind
        || identity.source_digest() != source.identity().digest()
        || identity.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(())
}

fn validate_related_session_identity(identity: StableEntityId) -> Result<()> {
    identity.validate_contract()?;
    if identity.entity_kind() != StableEntityKind::Session {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(())
}

fn validate_indexed_lineage_projection(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
    projection: &StoredLineageProjection,
) -> Result<()> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.event_id,
        &projection.event_id.to_string(),
        "event_id",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.event_identity_digest,
        &hex(&projection.event_id.digest()),
        "event_identity_digest",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.session_id,
        &projection.session_id.to_string(),
        "session_id",
    )?;
    if let Some(parent_session_id) = projection.parent_session_id {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.parent_session_id,
            &parent_session_id.to_string(),
            "parent_session_id",
        )?;
    }
    if let Some(root_session_id) = projection.root_session_id {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.root_session_id,
            &root_session_id.to_string(),
            "root_session_id",
        )?;
    }
    if let Some(relationship) = projection.session_relationship {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.provider_native_session_relationship,
            relationship.as_str(),
            "provider_native_session_relationship",
        )?;
    }
    if let Some(copy) = &projection.event_copy {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.event_copy_ancestor_session_id,
            &copy.ancestor_session_id.to_string(),
            "event_copy_ancestor_session_id",
        )?;
        validate_text_posting(
            segment,
            address.doc_id,
            fields.event_copy_ancestor_event_id,
            &copy.ancestor_event_id.to_string(),
            "event_copy_ancestor_event_id",
        )?;
        validate_text_posting(
            segment,
            address.doc_id,
            fields.event_copy_proof,
            copy_proof_str(copy.proof),
            "event_copy_proof",
        )?;
    }
    validate_u64_posting(
        segment,
        address.doc_id,
        fields.event_sequence,
        projection.event_sequence,
        "event_sequence",
    )?;

    let event = projection.event_id.as_uuid().as_u128();
    validate_fast_u64(
        segment,
        address.doc_id,
        "event_id_high",
        (event >> 64) as u64,
    )?;
    validate_fast_u64(segment, address.doc_id, "event_id_low", event as u64)?;
    let session = projection.session_id.as_uuid().as_u128();
    validate_fast_u64(
        segment,
        address.doc_id,
        "session_id_high",
        (session >> 64) as u64,
    )?;
    validate_fast_u64(segment, address.doc_id, "session_id_low", session as u64)?;
    validate_fast_u64(
        segment,
        address.doc_id,
        "event_sequence",
        projection.event_sequence,
    )?;
    Ok(())
}

fn validate_text_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    expected: &str,
    field_name: &'static str,
) -> Result<()> {
    validate_term_posting(
        segment,
        doc_id,
        field,
        Term::from_field_text(field, expected),
        field_name,
    )
}

fn validate_u64_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    expected: u64,
    field_name: &'static str,
) -> Result<()> {
    validate_term_posting(
        segment,
        doc_id,
        field,
        Term::from_field_u64(field, expected),
        field_name,
    )
}

fn validate_term_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    term: Term,
    field_name: &'static str,
) -> Result<()> {
    let inverted = segment.inverted_index(field)?;
    let term_info = inverted
        .get_term_info(&term)?
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    let mut postings =
        inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
    if postings.seek(doc_id) != doc_id {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(())
}

fn validate_fast_u64(
    segment: &SegmentReader,
    doc_id: DocId,
    field_name: &'static str,
    expected: u64,
) -> Result<()> {
    let column = segment.fast_fields().u64(field_name)?;
    let mut values = column.values_for_doc(doc_id);
    if values.next() != Some(expected) || values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(())
}

fn relationship_index(relationship: Option<ProviderNativeSessionRelationship>) -> usize {
    match relationship {
        Some(ProviderNativeSessionRelationship::Root) => 0,
        Some(ProviderNativeSessionRelationship::Delegated) => 1,
        Some(ProviderNativeSessionRelationship::Forked) => 2,
        Some(ProviderNativeSessionRelationship::ResumedFrom) => 3,
        Some(ProviderNativeSessionRelationship::WorkflowChild) => 4,
        None => 5,
    }
}

const RELATIONSHIP_ORDER: [Option<ProviderNativeSessionRelationship>; 6] = [
    Some(ProviderNativeSessionRelationship::Root),
    Some(ProviderNativeSessionRelationship::Delegated),
    Some(ProviderNativeSessionRelationship::Forked),
    Some(ProviderNativeSessionRelationship::ResumedFrom),
    Some(ProviderNativeSessionRelationship::WorkflowChild),
    None,
];

fn copy_proof_str(proof: ProviderNativeCopyProof) -> &'static str {
    match proof {
        ProviderNativeCopyProof::NativeEventIdentity => "native_event_identity",
        ProviderNativeCopyProof::NativeCopiedFromField => "native_copied_from_field",
        ProviderNativeCopyProof::NativeCallResultIdentity => "native_call_result_identity",
    }
}
