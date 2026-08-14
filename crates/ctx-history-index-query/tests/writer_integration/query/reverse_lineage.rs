use super::*;
use ctx_history_core::StableEntityId;

fn session_event(
    source: &SourceKey,
    native_session_id: &str,
    sequence: u64,
    body: &str,
) -> CoreRecord {
    super::super::document_for_session(source, native_session_id, sequence, body)
}

fn copied_event(
    mut event: CoreRecord,
    parent_session_id: StableEntityId,
    claimed_root_session_id: StableEntityId,
    relationship: SessionRelationshipKind,
    ancestor: &CoreRecord,
) -> CoreRecord {
    event
        .set_session_relationship(
            relationship,
            Some(parent_session_id),
            claimed_root_session_id,
        )
        .unwrap();
    event.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor.session_id),
        ancestor_event_id: Box::new(ancestor.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    event.validate_contract().unwrap();
    event
}

fn publish_records(source: &SourceKey, records: &[CoreRecord]) -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(super::super::certificate(source, 1, records.len() as u64))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn relationship_count(
    result: &CopiedEventLineage,
    relationship: SessionRelationshipKind,
) -> Option<u64> {
    result
        .relationship_counts
        .iter()
        .find(|count| count.session_relationship == relationship)
        .map(|count| count.observed_count)
}

#[test]
fn direct_copy_returns_child_owned_claim_and_resolved_target() {
    let source = source("reverse-lineage-direct.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copy = copied_event(
        session_event(&source, "child", 2, "copied"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let (_temp, index) = publish_records(&source, &[root.clone(), copy.clone()]);

    let result = index
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(result.generation_id, index.generation_id());
    assert_eq!(result.selected_event_id, root.event_id.as_uuid());
    assert_eq!(result.selected_session_id, Some(root.session_id));
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Resolved {
            event_id: root.event_id,
            session_id: root.session_id,
        }
    );
    assert_eq!(result.selected_depth, 0);
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(
        result.occurrences,
        vec![CopiedEventLineageOccurrence {
            event_id: copy.event_id,
            session_id: copy.session_id,
            copied_from_event_id: root.event_id,
            copied_from_session_id: root.session_id,
            parent_session_id: Some(root.session_id),
            claimed_root_session_id: root.session_id,
            session_relationship: SessionRelationshipKind::Forked,
            depth: 1,
        }]
    );
}

#[test]
fn selected_copy_resolves_forward_then_returns_breadth_first_claims() {
    let source = source("reverse-lineage-multihop.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let middle = copied_event(
        session_event(&source, "middle", 2, "middle"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Delegated,
        &root,
    );
    let leaf = copied_event(
        session_event(&source, "leaf", 3, "leaf"),
        middle.session_id,
        root.session_id,
        SessionRelationshipKind::ResumedFrom,
        &middle,
    );
    let (_temp, index) = publish_records(&source, &[leaf.clone(), root.clone(), middle.clone()]);

    let result = index
        .copied_event_lineage(leaf.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(result.selected_event_id, leaf.event_id.as_uuid());
    assert_eq!(result.selected_session_id, Some(leaf.session_id));
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Resolved {
            event_id: root.event_id,
            session_id: root.session_id,
        }
    );
    assert_eq!(result.selected_depth, 2);
    assert_eq!(result.exact_observed_count(), Some(2));
    assert_eq!(
        result
            .occurrences
            .iter()
            .map(|occurrence| (occurrence.event_id, occurrence.depth))
            .collect::<Vec<_>>(),
        vec![(middle.event_id, 1), (leaf.event_id, 2)]
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::Delegated),
        Some(1)
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::ResumedFrom),
        Some(1)
    );
}

#[test]
fn absent_target_is_unresolved_and_reverse_claims_remain_queryable() {
    let source = source("reverse-lineage-absent-target.jsonl");
    let absent = session_event(&source, "absent", 1, "not published");
    let copy = copied_event(
        session_event(&source, "child", 2, "copied"),
        absent.session_id,
        absent.session_id,
        SessionRelationshipKind::Forked,
        &absent,
    );
    let (_temp, index) = publish_records(&source, std::slice::from_ref(&copy));

    let result = index
        .copied_event_lineage(absent.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(result.selected_event_id, absent.event_id.as_uuid());
    assert_eq!(result.selected_session_id, None);
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Unresolved {
            event_id: absent.event_id.as_uuid(),
            session_id: None,
        }
    );
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(result.occurrences[0].event_id, copy.event_id);
    assert_eq!(result.occurrences[0].copied_from_event_id, absent.event_id);
    assert!(index
        .search_event_candidates("copied", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn selected_copy_with_absent_target_is_unresolved_and_keeps_its_reverse_claim() {
    let source = source("reverse-lineage-selected-copy-absent-target.jsonl");
    let absent = session_event(&source, "absent", 1, "not published");
    let copy = copied_event(
        session_event(&source, "child", 2, "selected copy"),
        absent.session_id,
        absent.session_id,
        SessionRelationshipKind::Forked,
        &absent,
    );
    let (_temp, index) = publish_records(&source, std::slice::from_ref(&copy));

    let result = index
        .copied_event_lineage(copy.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();

    assert_eq!(result.selected_session_id, Some(copy.session_id));
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Unresolved {
            event_id: absent.event_id.as_uuid(),
            session_id: Some(absent.session_id),
        }
    );
    assert_eq!(result.selected_depth, 1);
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(result.occurrences[0].event_id, copy.event_id);
}

#[test]
fn missing_parent_publishes_but_forward_resolution_is_unresolved() {
    let source = source("reverse-lineage-missing-parent.jsonl");
    let target = session_event(&source, "target", 1, "published target");
    let missing_parent = session_event(&source, "missing-parent", 2, "not published");
    let copy = copied_event(
        session_event(&source, "child", 3, "copied"),
        missing_parent.session_id,
        target.session_id,
        SessionRelationshipKind::Delegated,
        &target,
    );
    let (_temp, index) = publish_records(&source, &[target.clone(), copy.clone()]);

    let result = index
        .copied_event_lineage(copy.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Unresolved {
            event_id: target.event_id.as_uuid(),
            session_id: Some(target.session_id),
        }
    );
    assert_eq!(result.selected_depth, 1);
    assert_eq!(result.occurrences[0].event_id, copy.event_id);
}

#[test]
fn copied_event_cycle_publishes_and_resolves_as_cyclic() {
    let source = source("reverse-lineage-cycle.jsonl");
    let claimed_root = session_event(&source, "claimed-root", 1, "not published");
    let mut first = session_event(&source, "first", 2, "first");
    let mut second = session_event(&source, "second", 3, "second");
    first
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second.session_id),
            claimed_root.session_id,
        )
        .unwrap();
    second
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first.session_id),
            claimed_root.session_id,
        )
        .unwrap();
    first.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(second.session_id),
        ancestor_event_id: Box::new(second.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    second.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(first.session_id),
        ancestor_event_id: Box::new(first.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    first.validate_contract().unwrap();
    second.validate_contract().unwrap();
    let (_temp, index) = publish_records(&source, &[first.clone(), second.clone()]);

    let result = index
        .copied_event_lineage(first.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Cyclic {
            event_id: first.event_id,
            session_id: first.session_id,
        }
    );
    assert_eq!(result.selected_depth, 2);
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(result.occurrences[0].event_id, second.event_id);
}

#[test]
fn parent_only_cycle_publishes_and_resolves_as_cyclic() {
    let source = source("reverse-lineage-parent-only-cycle.jsonl");
    let target = session_event(&source, "target", 1, "target");
    let mut child = session_event(&source, "child", 2, "copied child");
    let mut parent = session_event(&source, "parent", 3, "cyclic parent");
    child
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(parent.session_id),
            target.session_id,
        )
        .unwrap();
    parent
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(child.session_id),
            target.session_id,
        )
        .unwrap();
    child.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(target.session_id),
        ancestor_event_id: Box::new(target.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    child.validate_contract().unwrap();
    parent.validate_contract().unwrap();
    let (_temp, index) = publish_records(&source, &[target, child.clone(), parent]);

    let result = index
        .copied_event_lineage(child.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();

    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Cyclic {
            event_id: child.event_id,
            session_id: child.session_id,
        }
    );
    assert_eq!(result.selected_depth, 1);
}

#[test]
fn preview_and_posting_bounds_keep_exactness_truthful() {
    let source = source("reverse-lineage-bounds.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copies = (2..=5)
        .map(|sequence| {
            copied_event(
                session_event(&source, &format!("child-{sequence}"), sequence, "copy"),
                root.session_id,
                root.session_id,
                SessionRelationshipKind::Forked,
                &root,
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![root.clone()];
    records.extend(copies);
    let (_temp, index) = publish_records(&source, &records);

    let preview = index
        .copied_event_lineage(root.event_id.as_uuid(), SEARCH_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(preview.returned, 3);
    assert_eq!(preview.exact_observed_count(), Some(4));

    let bounded = index
        .copied_event_lineage(
            root.event_id.as_uuid(),
            CopiedEventLineagePolicy::new(20, 1),
        )
        .unwrap();
    assert_eq!(bounded.observed_count, 1);
    assert!(bounded.truncated);
    assert_eq!(bounded.exact_observed_count(), None);
}

#[test]
fn deleted_exact_identity_postings_hit_the_shared_typed_absolute_bound() {
    let source = source("reverse-lineage-deleted-identity-postings.jsonl");
    let ancestor = session_event(&source, "ancestor", 1, "ancestor");
    let selected = copied_event(
        session_event(&source, "selected", 2, "selected"),
        ancestor.session_id,
        ancestor.session_id,
        SessionRelationshipKind::Forked,
        &ancestor,
    );
    let survivor = session_event(&source, "survivor", 3, "survivor");
    let (temp, baseline) = publish_records(
        &source,
        &[ancestor.clone(), selected.clone(), survivor.clone()],
    );
    let index = open_unverified_generation(temp.path()).0.index().clone();
    let event_id = required_field(&index.schema(), "event_id").unwrap();

    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    for _ in 0..=MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS {
        writer
            .add_document(super::super::indexed_document(ancestor.clone()))
            .unwrap();
    }
    writer
        .add_document(super::super::indexed_document(survivor))
        .unwrap();
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();

    let mut deleting = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    deleting.set_merge_policy(Box::<NoMergePolicy>::default());
    deleting.delete_term(Term::from_field_text(
        event_id,
        &ancestor.event_id.to_string(),
    ));
    deleting.commit().unwrap();
    deleting.wait_merging_threads().unwrap();

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    let searcher = reader.searcher();
    assert!(
        searcher
            .segment_readers()
            .iter()
            .map(|segment| segment.num_deleted_docs() as usize)
            .sum::<usize>()
            > MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS
    );
    let deleted_heavy = baseline.test_with_searcher(searcher);

    assert!(matches!(
        deleted_heavy.copied_event_lineage(
            selected.event_id.as_uuid(),
            SHOW_COPIED_EVENT_LINEAGE_POLICY
        ),
        Err(
            IndexError::CopiedEventLineageEventAndSessionIdentityPostingVisitLimitExceeded {
                maximum: MAX_COPIED_EVENT_LINEAGE_EVENT_AND_SESSION_IDENTITY_POSTING_VISITS
            }
        )
    ));
}

#[test]
fn depth_1024_is_complete_and_a_deeper_edge_truncates_truthfully() {
    let source = source("reverse-lineage-depth-bound.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let mut records = vec![root.clone()];
    let mut previous = root.clone();
    for depth in 1..=MAX_COPIED_EVENT_LINEAGE_DEPTH {
        let next = copied_event(
            session_event(&source, &format!("depth-{depth}"), depth as u64 + 1, "copy"),
            previous.session_id,
            root.session_id,
            SessionRelationshipKind::Forked,
            &previous,
        );
        records.push(next.clone());
        previous = next;
    }
    let (temp, baseline) = publish_records(&source, std::slice::from_ref(&root));
    let index = open_unverified_generation(temp.path()).0.index().clone();
    drop(baseline);
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(
            &source,
            2,
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 1,
        )])
        .unwrap(),
        std::slice::from_ref(&source),
        records
            .iter()
            .cloned()
            .map(super::super::indexed_document)
            .collect(),
    );
    let exact_depth = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let complete = exact_depth
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(
        complete.exact_observed_count(),
        Some(MAX_COPIED_EVENT_LINEAGE_DEPTH as u64)
    );
    assert_eq!(
        complete.returned,
        SHOW_COPIED_EVENT_LINEAGE_POLICY.maximum_occurrences
    );
    let selected_at_boundary = exact_depth
        .copied_event_lineage(
            previous.event_id.as_uuid(),
            SHOW_COPIED_EVENT_LINEAGE_POLICY,
        )
        .unwrap();
    assert_eq!(
        selected_at_boundary.selected_depth,
        MAX_COPIED_EVENT_LINEAGE_DEPTH
    );
    assert_eq!(
        selected_at_boundary.resolution,
        CopiedEventLineageResolution::Resolved {
            event_id: root.event_id,
            session_id: root.session_id,
        }
    );
    assert_eq!(
        selected_at_boundary.exact_observed_count(),
        Some(MAX_COPIED_EVENT_LINEAGE_DEPTH as u64)
    );
    drop(exact_depth);

    let beyond = copied_event(
        session_event(
            &source,
            "beyond-depth-bound",
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 2,
            "copy",
        ),
        previous.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &previous,
    );
    let mut forged_documents = records
        .into_iter()
        .map(super::super::indexed_document)
        .collect::<Vec<_>>();
    forged_documents.push(super::super::indexed_document(beyond));
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(
            &source,
            3,
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 2,
        )])
        .unwrap(),
        std::slice::from_ref(&source),
        forged_documents,
    );
    let forged = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let truncated = forged
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert!(truncated.truncated);
    assert_eq!(
        truncated.observed_count,
        MAX_COPIED_EVENT_LINEAGE_DEPTH as u64
    );
    assert_eq!(truncated.exact_observed_count(), None);
}

#[test]
fn forged_inverse_origin_projection_fails_closed() {
    let source = source("reverse-lineage-forged-origin.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let other = session_event(&source, "root", 2, "other root event");
    let copy = copied_event(
        session_event(&source, "child", 3, "copy"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &other,
    );
    let (temp, pinned) = publish_records(&source, &[root.clone(), other.clone(), copy.clone()]);
    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let target = fields.origin_event_id;
    let complete = super::super::indexed_document(copy);
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != target {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(target, root.event_id.as_uuid().to_string());
    let index = searcher.index().clone();
    drop(searcher);
    drop(pinned);
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(&source, 2, 3)]).unwrap(),
        std::slice::from_ref(&source),
        vec![
            super::super::indexed_document(root.clone()),
            super::super::indexed_document(other),
            forged,
        ],
    );
    let forged_index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert!(matches!(
        forged_index
            .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY),
        Err(IndexError::InvalidStoredDocumentField("origin_event_id"))
    ));
}

#[test]
fn open_reader_remains_generation_pinned_across_replacement() {
    let source = source("reverse-lineage-generation-pinned.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copy = copied_event(
        session_event(&source, "child", 2, "copy"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let (temp, pinned) = publish_records(&source, &[root.clone(), copy]);
    let pinned_generation = pinned.generation_id().to_owned();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement.add_core_record(root.clone()).unwrap();
    replacement
        .certify_source(super::super::certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();
    let active = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let retained_result = pinned
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    let active_result = active
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(retained_result.generation_id, pinned_generation);
    assert_eq!(retained_result.exact_observed_count(), Some(1));
    assert_ne!(active_result.generation_id, pinned_generation);
    assert_eq!(active_result.exact_observed_count(), Some(0));
}
