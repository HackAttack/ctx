#[test]
fn certificate_count_mismatch_is_rejected_before_commit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    let error = writer
        .certify_source(certificate(&source, 1, 2))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::SourceDocumentCountMismatch { .. }
    ));
}

#[test]
fn duplicate_event_identity_is_rejected_by_prepublication_term_audit() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let duplicate = document(&source, 1, "first");
    writer.add_core_record(duplicate.clone()).unwrap();
    writer.add_core_record(duplicate).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    let error = writer.commit(|_| true).unwrap_err();
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
    assert!(load_active_generation_pointer(temp.path())
        .unwrap()
        .is_none());
}

#[test]
fn copied_event_claims_publish_with_their_exact_declared_target() {
    let temp = tempdir().unwrap();
    let source = source("valid-copy.jsonl");
    let original = document_for_session(&source, "root", 1, "original");
    let mut copy = document_for_session(&source, "child", 2, "copy");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(original.session_id),
        original.session_id,
    )
    .unwrap();
    copy.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    });

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(original).unwrap();
    writer.add_core_record(copy).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn copied_event_with_a_missing_target_publishes_as_an_unresolved_reference() {
    let temp = tempdir().unwrap();
    let source = source("missing-copy.jsonl");
    let missing = document_for_session(&source, "root", 1, "missing");
    let mut copy = document_for_session(&source, "child", 2, "copy");
    copy.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(missing.session_id),
        missing.session_id,
    )
    .unwrap();
    copy.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: missing.session_id,
        ancestor_event_id: missing.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    });

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(copy).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        1
    );
}

#[test]
fn deleted_session_terms_without_live_postings_do_not_block_publication() {
    let temp = tempdir().unwrap();
    let removed_source = source("removed-session.jsonl");
    let removed = document_for_session(&removed_source, "removed", 1, "removed session");

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(removed_source.clone()).unwrap();
    initial.add_core_record(removed).unwrap();
    initial
        .certify_source(certificate(&removed_source, 1, 1))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) = deletion_evidence(&removed_source, 2);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    deleting.commit(|_| true).unwrap();

    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        0
    );
}

#[test]
fn deleting_a_parent_keeps_the_child_as_an_unresolved_reference() {
    let temp = tempdir().unwrap();
    let parent_source = source("parent-session.jsonl");
    let child_source = source("child-session.jsonl");
    let parent = document_for_session(&parent_source, "parent", 1, "parent session");
    let mut child = document_for_session(&child_source, "child", 1, "child session");
    child
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(parent.session_id),
            parent.session_id,
        )
        .unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, record) in [(&parent_source, parent), (&child_source, child)] {
        initial.begin_source(source.clone()).unwrap();
        initial.add_core_record(record).unwrap();
        initial.certify_source(certificate(source, 1, 1)).unwrap();
    }
    initial.commit(|_| true).unwrap();

    let (deletion, inventory) =
        deletion_evidence_with_retained(&parent_source, 2, vec![child_source]);
    let mut deleting = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    deleting.delete_source(deletion, inventory).unwrap();
    deleting.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        1
    );
}

#[test]
fn direct_copy_chain_claims_publish_without_graph_resolution() {
    let temp = tempdir().unwrap();
    let source = source("copy-chain.jsonl");
    let original = document_for_session(&source, "root", 1, "original");
    let mut middle = document_for_session(&source, "middle", 2, "middle copy");
    middle
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(original.session_id),
            original.session_id,
        )
        .unwrap();
    middle.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: original.session_id,
        ancestor_event_id: original.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    });
    let mut leaf = document_for_session(&source, "leaf", 3, "leaf copy");
    leaf.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(middle.session_id),
        original.session_id,
    )
    .unwrap();
    leaf.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: middle.session_id,
        ancestor_event_id: middle.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    });

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [original, middle, leaf] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn cyclic_session_relationship_claims_publish_without_graph_resolution() {
    let temp = tempdir().unwrap();
    let source = source("session-cycle.jsonl");
    let root = document_for_session(&source, "root", 1, "root");
    let mut first = document_for_session(&source, "first", 2, "first");
    let mut second = document_for_session(&source, "second", 3, "second");
    first
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second.session_id),
            root.session_id,
        )
        .unwrap();
    second
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first.session_id),
            root.session_id,
        )
        .unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [root, first, second] {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        3
    );
}

fn claimed_child_event(
    source: &SourceKey,
    sequence: u64,
    body: &str,
    parent: Option<StableEntityId>,
) -> CoreRecord {
    let mut event = document_for_session(source, "child", sequence, body);
    if let Some(parent) = parent {
        event
            .set_session_relationship(SessionRelationshipKind::Forked, Some(parent), parent)
            .unwrap();
    }
    event
}

fn publish_fresh_claims(
    source: &SourceKey,
    claims: &[Option<StableEntityId>],
) -> std::result::Result<(), IndexError> {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (offset, claim) in claims.iter().copied().enumerate() {
        writer.add_core_record(claimed_child_event(
            source,
            u64::try_from(offset + 1).unwrap(),
            "fresh claim",
            claim,
        ))?;
    }
    writer
        .certify_source(certificate(source, 1, claims.len() as u64))
        .unwrap();
    writer.commit(|_| true).map(|_| ())
}

fn append_claim(
    root: &Path,
    source: &SourceKey,
    revision: u8,
    sequence: u64,
    claim: Option<StableEntityId>,
) -> std::result::Result<(), IndexError> {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(source.clone())?.clone();
    writer.add_core_record(claimed_child_event(source, sequence, "append claim", claim))?;
    writer.certify_source_append(CertifiedSourceAppend::certify(
        &base,
        appendable_certificate(source, revision, sequence, sequence * 10),
        (sequence - 1) * 10,
        [revision - 1; 32],
    )?)?;
    writer.commit(|_| true).map(|_| ())
}

#[test]
fn fresh_session_claim_merge_accepts_absent_then_positive_and_propagates_it() {
    let source = source("fresh-absent-positive.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    let conflicting = document_for_session(&source, "other-parent", 11, "not published").session_id;
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(claimed_child_event(&source, 1, "unknown", None))
        .unwrap();
    writer
        .add_core_record(claimed_child_event(&source, 2, "positive", Some(parent)))
        .unwrap();
    assert!(matches!(
        writer.add_core_record(claimed_child_event(
            &source,
            3,
            "conflicting positive",
            Some(conflicting),
        )),
        Err(IndexError::ConflictingProviderNativeSessionClaim(_))
    ));
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn fresh_session_claim_merge_accepts_positive_then_absent() {
    let source = source("fresh-positive-absent.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    publish_fresh_claims(&source, &[Some(parent), None]).unwrap();
}

#[test]
fn fresh_session_claim_merge_accepts_equal_positive_claims() {
    let source = source("fresh-equal-positive.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    publish_fresh_claims(&source, &[Some(parent), Some(parent)]).unwrap();
}

#[test]
fn fresh_session_claim_merge_rejects_conflicting_positive_claims() {
    let temp = tempdir().unwrap();
    let source = source("contradictory-session-claims.jsonl");
    let first_parent = document_for_session(&source, "first-parent", 1, "not published");
    let second_parent = document_for_session(&source, "second-parent", 2, "not published");
    let mut first = document_for_session(&source, "child", 3, "first child event");
    let mut second = document_for_session(&source, "child", 4, "second child event");
    first
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first_parent.session_id),
            first_parent.session_id,
        )
        .unwrap();
    second
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second_parent.session_id),
            second_parent.session_id,
        )
        .unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first).unwrap();
    let error = writer.add_core_record(second).unwrap_err();
    assert!(matches!(
        error,
        IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields"
        )
    ));
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
}

#[test]
fn append_recovery_claim_merge_accepts_absent_then_positive_and_propagates_it() {
    let temp = tempdir().unwrap();
    let source = source("append-absent-positive.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    let conflicting = document_for_session(&source, "other-parent", 11, "not published").session_id;

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(claimed_child_event(&source, 1, "unknown base", None))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    append_claim(temp.path(), &source, 2, 2, Some(parent)).unwrap();
    assert!(matches!(
        append_claim(temp.path(), &source, 3, 3, Some(conflicting)),
        Err(IndexError::ConflictingProviderNativeSessionClaim(_))
    ));
}

#[test]
fn append_recovery_claim_merge_accepts_positive_then_absent() {
    let temp = tempdir().unwrap();
    let source = source("append-positive-absent.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(claimed_child_event(
            &source,
            1,
            "positive base",
            Some(parent),
        ))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();
    append_claim(temp.path(), &source, 2, 2, None).unwrap();
}

#[test]
fn append_recovery_claim_merge_accepts_equal_positive_claims() {
    let temp = tempdir().unwrap();
    let source = source("append-equal-positive.jsonl");
    let parent = document_for_session(&source, "parent", 10, "not published").session_id;
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(claimed_child_event(
            &source,
            1,
            "positive base",
            Some(parent),
        ))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();
    append_claim(temp.path(), &source, 2, 2, Some(parent)).unwrap();
}

#[test]
fn append_recovery_claim_merge_rejects_conflicting_positive_claims() {
    let temp = tempdir().unwrap();
    let source = source("append-session-claim.jsonl");
    let first_parent = document_for_session(&source, "first-parent", 10, "not published");
    let second_parent = document_for_session(&source, "second-parent", 11, "not published");
    let mut base_event = document_for_session(&source, "child", 1, "base child event");
    base_event
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first_parent.session_id),
            first_parent.session_id,
        )
        .unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(base_event).unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    let mut contradictory = document_for_session(&source, "child", 2, "invalid append");
    contradictory
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second_parent.session_id),
            second_parent.session_id,
        )
        .unwrap();
    assert!(matches!(
        append.add_core_record(contradictory),
        Err(IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields"
        ))
    ));

    let mut valid = document_for_session(&source, "child", 2, "valid append");
    valid
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(first_parent.session_id),
            first_parent.session_id,
        )
        .unwrap();
    append.add_core_record(valid).unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    append.commit(|_| true).unwrap();
}

#[test]
fn replacement_may_change_a_session_claim_from_deleted_source_history() {
    let temp = tempdir().unwrap();
    let source = source("replacement-session-claim.jsonl");
    let first_parent = document_for_session(&source, "first-parent", 10, "not published");
    let second_parent = document_for_session(&source, "second-parent", 11, "not published");
    let mut old = document_for_session(&source, "child", 1, "old child event");
    old.set_session_relationship(
        SessionRelationshipKind::Forked,
        Some(first_parent.session_id),
        first_parent.session_id,
    )
    .unwrap();

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(old).unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    let mut current = document_for_session(&source, "child", 2, "current child event");
    current
        .set_session_relationship(
            SessionRelationshipKind::Forked,
            Some(second_parent.session_id),
            second_parent.session_id,
        )
        .unwrap();
    replacement.add_core_record(current).unwrap();
    replacement
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.document_count(), 1);
    assert_eq!(index.count_term("current").unwrap(), 1);
}

#[test]
fn replacement_rejects_a_compact_session_collision_owned_by_a_retained_source() {
    const IDENTITY_DIGEST_OFFSET: usize = 3;
    const IDENTITY_SOURCE_DIGEST_OFFSET: usize = IDENTITY_DIGEST_OFFSET + 32;
    const IDENTITY_UUID_OFFSET: usize = StableEntityId::CANONICAL_LEN - 16;

    let temp = tempdir().unwrap();
    let retained_source = source("retained-session-owner.jsonl");
    let replacement_source = source("replacement-session-owner.jsonl");
    let retained_record = document(&retained_source, 1, "retained session");
    let retained_session = retained_record.session_id;
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(retained_source.clone()).unwrap();
    initial.add_core_record(retained_record).unwrap();
    initial
        .certify_source(certificate(&retained_source, 1, 1))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement
        .begin_source(replacement_source.clone())
        .unwrap();
    let mut prepared = replacement
        .core_record_preparer()
        .prepare(document(&replacement_source, 1, "colliding replacement"))
        .unwrap();

    // Production preparation cannot manufacture a compact collision. This
    // crate-local test hook preserves the candidate source authority while
    // forcing a different full digest to share the retained session's UUID.
    let candidate_session = prepared.test_identity_facts_mut().session.session_id;
    let mut encoded = candidate_session.encode_canonical().unwrap();
    encoded[IDENTITY_DIGEST_OFFSET..IDENTITY_DIGEST_OFFSET + 16]
        .copy_from_slice(&retained_session.digest()[..16]);
    encoded[IDENTITY_UUID_OFFSET..].copy_from_slice(retained_session.as_uuid().as_bytes());
    let colliding_session = StableEntityId::decode_canonical(&encoded).unwrap();
    assert_eq!(colliding_session.as_uuid(), retained_session.as_uuid());
    assert_ne!(colliding_session.digest(), retained_session.digest());
    assert_eq!(
        &encoded[IDENTITY_SOURCE_DIGEST_OFFSET..IDENTITY_SOURCE_DIGEST_OFFSET + 32],
        &replacement_source.identity().digest()
    );
    prepared.test_identity_facts_mut().session.session_id = colliding_session;

    assert!(matches!(
        replacement.add_prepared_core_record(prepared),
        Err(IndexError::CompactIdentityCollision {
            kind: "session",
            ..
        })
    ));
}

#[test]
fn candidate_deletion_ignores_a_compact_session_collision_owned_only_by_the_deleted_source() {
    const IDENTITY_DIGEST_OFFSET: usize = 3;
    const IDENTITY_SOURCE_DIGEST_OFFSET: usize = IDENTITY_DIGEST_OFFSET + 32;
    const IDENTITY_UUID_OFFSET: usize = StableEntityId::CANONICAL_LEN - 16;

    let temp = tempdir().unwrap();
    let deleted_source = source("deleted-session-owner.jsonl");
    let candidate_source = source("candidate-session-owner.jsonl");
    let deleted_record = document(&deleted_source, 1, "deleted session");
    let deleted_session = deleted_record.session_id;
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(deleted_source.clone()).unwrap();
    initial.add_core_record(deleted_record).unwrap();
    initial
        .certify_source(certificate(&deleted_source, 1, 1))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let (deletion, inventory) =
        deletion_evidence_with_retained(&deleted_source, 2, vec![candidate_source.clone()]);
    candidate.delete_source(deletion, inventory).unwrap();
    candidate.begin_source(candidate_source.clone()).unwrap();
    let mut prepared = candidate
        .core_record_preparer()
        .prepare(document(&candidate_source, 1, "colliding candidate"))
        .unwrap();

    let candidate_session = prepared.test_identity_facts_mut().session.session_id;
    let mut encoded = candidate_session.encode_canonical().unwrap();
    encoded[IDENTITY_DIGEST_OFFSET..IDENTITY_DIGEST_OFFSET + 16]
        .copy_from_slice(&deleted_session.digest()[..16]);
    encoded[IDENTITY_UUID_OFFSET..].copy_from_slice(deleted_session.as_uuid().as_bytes());
    let colliding_session = StableEntityId::decode_canonical(&encoded).unwrap();
    assert_eq!(colliding_session.as_uuid(), deleted_session.as_uuid());
    assert_ne!(colliding_session.digest(), deleted_session.digest());
    assert_eq!(
        &encoded[IDENTITY_SOURCE_DIGEST_OFFSET..IDENTITY_SOURCE_DIGEST_OFFSET + 32],
        &candidate_source.identity().digest()
    );
    prepared.test_identity_facts_mut().session.session_id = colliding_session;

    candidate.add_prepared_core_record(prepared).unwrap();
    candidate
        .certify_source(certificate(&candidate_source, 1, 1))
        .unwrap();
    candidate.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.document_count(), 1);
    assert_eq!(index.count_term("colliding").unwrap(), 1);
}

#[test]
fn same_source_replacement_rejects_a_compact_session_collision_without_poisoning_the_base() {
    const IDENTITY_DIGEST_OFFSET: usize = 3;
    const IDENTITY_UUID_OFFSET: usize = StableEntityId::CANONICAL_LEN - 16;

    let temp = tempdir().unwrap();
    let source = source("same-source-session-collision.jsonl");
    let retained = document_for_session(&source, "retained", 1, "retained session");
    let retained_session = retained.session_id;
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial.add_core_record(retained).unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    let mut prepared = replacement
        .core_record_preparer()
        .prepare(document_for_session(
            &source,
            "different",
            1,
            "colliding replacement",
        ))
        .unwrap();
    let candidate_session = prepared.test_identity_facts_mut().session.session_id;
    let mut encoded = candidate_session.encode_canonical().unwrap();
    encoded[IDENTITY_DIGEST_OFFSET..IDENTITY_DIGEST_OFFSET + 16]
        .copy_from_slice(&retained_session.digest()[..16]);
    encoded[IDENTITY_UUID_OFFSET..].copy_from_slice(retained_session.as_uuid().as_bytes());
    prepared.test_identity_facts_mut().session.session_id =
        StableEntityId::decode_canonical(&encoded).unwrap();

    assert!(matches!(
        replacement.add_prepared_core_record(prepared),
        Err(IndexError::CompactIdentityCollision {
            kind: "session",
            ..
        })
    ));
    drop(replacement);
    assert!(!temp
        .path()
        .join("active-generation-rebuild-required.json")
        .exists());
    GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
}

#[test]
fn verified_generation_rejects_a_forged_duplicate_event_identity() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, _) = open_unverified_generation(temp.path());
    let addresses = searcher.search(&AllQuery, &DocSetCollector).unwrap();
    let address = addresses.into_iter().next().unwrap();
    let duplicate = indexed_document(decoded_stored_core(&searcher, address));
    let index = searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 2)]).unwrap(),
        &[],
        vec![duplicate],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("duplicate event generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::DuplicateEventIdentity(_)));
}

#[test]
fn verified_generation_rejects_forged_source_ownership() {
    let temp = tempdir().unwrap();
    let first = source("first.jsonl");
    let second = source("second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(first.clone()).unwrap();
    writer.add_core_record(document(&first, 1, "body")).unwrap();
    writer.certify_source(certificate(&first, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = indexed_document(decoded_stored_core(&searcher, address));
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.source_key {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(fields.source_key, source_token(&second));
    let index = searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&second, 2, 1)]).unwrap(),
        std::slice::from_ref(&first),
        vec![forged],
    );

    let (searcher, manifest) = open_unverified_generation(temp.path());
    assert!(matches!(
        verify_searcher(&searcher, &manifest),
        Err(IndexError::InvalidStoredDocumentField("core_record"))
    ));
    let error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("source ownership mismatch unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::InvalidStoredDocumentField("core_record")
    ));
}

#[test]
fn verified_generation_rejects_malformed_stored_core_during_exhaustive_audit() {
    let temp = tempdir().unwrap();
    let source = source("malformed-core.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let event = document(&source, 1, "complete body");
    writer.add_core_record(event).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, _) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let document = searcher.doc::<TantivyDocument>(address).unwrap();
    let mut forged = TantivyDocument::default();
    for (field, value) in document.field_values() {
        if field != fields.core_record && field != fields.core_record_encoded_bytes {
            forged.add_field_value(field, value);
        }
    }
    forged.add_u64(fields.core_record_encoded_bytes, 1);
    forged.add_bytes(fields.core_record, b"{");
    let index = searcher.index().clone();
    publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![certificate(&source, 2, 1)]).unwrap(),
        std::slice::from_ref(&source),
        vec![forged],
    );

    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::CoreRecord(_))
    ));
}

#[test]
fn document_identity_kinds_are_checked() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.event_id = invalid.session_id;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn document_identities_must_belong_to_the_document_source() {
    let temp = tempdir().unwrap();
    let first = source("first");
    let second = source("second");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(second.clone()).unwrap();
    let mut invalid = document(&first, 1, "body");
    invalid.source = second;
    let error = writer.add_core_record(invalid).unwrap_err();
    assert!(matches!(error, IndexError::CoreRecord(_)));
}

#[test]
fn literal_facts_keep_an_empty_normalized_body_useful() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut invalid = document(&source, 1, "body");
    invalid.content.normalized_body = Some(String::new());
    writer.add_core_record(invalid).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.count_term("main").unwrap(), 1);
}

#[test]
fn invalid_memory_budget_has_no_filesystem_side_effect() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("not-created");
    let error = match GenerationWriter::open(
        &root,
        WriterOptions {
            indexer_threads: 2,
            memory_bytes: 1,
        },
    ) {
        Ok(_) => panic!("invalid memory budget unexpectedly opened an index"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::IndexMemoryTooSmall { .. }));
    assert!(!root.exists());
}
