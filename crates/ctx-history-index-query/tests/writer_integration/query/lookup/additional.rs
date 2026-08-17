use super::*;

#[test]
fn bounded_core_event_batch_is_complete_and_requested_ordered() {
    let temp = tempdir().unwrap();
    let source = source("bounded-event-batch.jsonl");
    let first = document(&source, 1, "first complete body");
    let second = document(&source, 2, "second complete body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let coordinates = index
        .session_event_coordinates(first.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        coordinates
            .iter()
            .map(|coordinate| coordinate.event_id)
            .collect::<Vec<_>>(),
        vec![first.event_id.as_uuid(), second.event_id.as_uuid()]
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "session selection metadata must not decode complete Core bodies"
    );

    let requested = [second.event_id.as_uuid(), first.event_id.as_uuid()];
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let bounded_batch = index
        .core_events_by_ids_with_budget(&requested, requested.len(), DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap()
        .unwrap();
    assert_eq!(
        bounded_batch
            .items
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert!(bounded_batch.encoded_core_bytes >= bounded_batch.content_bytes);
    assert_eq!(
        bounded_batch.content_bytes,
        "first complete body".len() + "second complete body".len()
    );

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let records = index
        .core_events_by_ids_if_bounded(&requested, requested.len(), usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.core_record.content.meaningful_text())
            .collect::<Vec<_>>(),
        vec!["second complete body", "first complete body",]
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        2,
        "each requested event must materialize exactly one stored Core document"
    );

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    assert!(index
        .core_events_by_ids_if_bounded(&requested, requested.len() - 1, usize::MAX)
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "an oversized request must be declined before querying stored documents"
    );

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    assert!(matches!(
        index.core_events_by_ids_if_bounded(
            &[first.event_id.as_uuid(), first.event_id.as_uuid()],
            2,
            usize::MAX,
        ),
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "duplicate requested IDs must be rejected before querying stored documents"
    );

    assert!(index
        .core_events_by_ids_if_bounded(&[first.event_id.as_uuid(), Uuid::nil()], 2, usize::MAX,)
        .unwrap()
        .is_none());
    assert!(index
        .core_events_by_ids_if_bounded(&[], 0, 0)
        .unwrap()
        .unwrap()
        .is_empty());
}

#[test]
fn strict_core_event_stream_selects_once_and_materializes_in_requested_order() {
    let temp = tempdir().unwrap();
    let source = source("strict-streaming-events.jsonl");
    let first = document(&source, 1, "first complete body");
    let second = document(&source, 2, "second complete body");
    let third = document(&source, 3, "third complete body");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.add_core_record(third.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let requested = [
        third.event_id.as_uuid(),
        first.event_id.as_uuid(),
        second.event_id.as_uuid(),
    ];
    ctx_history_index_query::reset_core_event_id_selection_queries();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let mut stream = index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &requested,
            requested.len(),
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        ctx_history_index_query::core_event_id_selection_queries(),
        1
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );

    for (offset, expected) in [third, first, second].into_iter().enumerate() {
        let actual = stream.next().unwrap().unwrap();
        assert_eq!(actual.core_record, expected);
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            offset + 1
        );
    }
    assert!(stream.next().is_none());
    assert_eq!(
        ctx_history_index_query::core_event_id_selection_queries(),
        1
    );
    drop(stream);

    ctx_history_index_query::reset_core_event_id_selection_queries();
    let duplicate = index.stream_core_events_by_ids_with_strict_per_record_budget(
        &[requested[0], requested[0]],
        2,
        DEFAULT_CORE_EVENT_PAGE_BUDGET,
    );
    assert!(matches!(
        duplicate,
        Err(IndexError::DuplicateEventIdentity(_))
    ));
    assert_eq!(
        ctx_history_index_query::core_event_id_selection_queries(),
        0
    );

    ctx_history_index_query::reset_core_event_id_selection_queries();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    assert!(index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &[requested[0], Uuid::nil()],
            2,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::core_event_id_selection_queries(),
        1
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
}

#[test]
fn strict_core_event_batch_rejects_fast_content_overflow_before_stored_materialization() {
    let temp = tempdir().unwrap();
    let source = source("strict-content-preflight.jsonl");
    let records = (1..=3)
        .map(|sequence| document(&source, sequence, "content over the strict budget"))
        .collect::<Vec<_>>();
    let requested = records
        .iter()
        .map(|record| record.event_id.as_uuid())
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_budget(
            &requested,
            requested.len(),
            CoreEventPageBudget::new(ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES, 1),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &requested,
            requested.len(),
            CoreEventPageBudget::new(ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES, 1),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
}

#[test]
fn strict_core_event_batch_rejects_encoded_heavy_candidates_before_materialization() {
    let temp = tempdir().unwrap();
    let source = source("strict-encoded-preflight.jsonl");
    let escaped_metadata = "\u{0001}".repeat(16 * 1024);
    let records = (1..=3)
        .map(|sequence| {
            let mut record = document(&source, sequence, "small");
            replace_literal_fact(
                &mut record,
                LiteralFactKind::Branch,
                escaped_metadata.clone(),
            );
            record.validate_contract().unwrap();
            assert!(record.encode_stored().unwrap().len() > 64 * 1024);
            record
        })
        .collect::<Vec<_>>();
    let requested = records
        .iter()
        .map(|record| record.event_id.as_uuid())
        .collect::<Vec<_>>();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_budget(
            &requested,
            requested.len(),
            CoreEventPageBudget::new(1, 3 * "small".len()),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
}

#[test]
fn strict_core_event_batch_aggregate_overflow_skips_all_stored_documents_and_decodes() {
    let temp = tempdir().unwrap();
    let source = source("strict-zero-encoded-remainder.jsonl");
    let first = document(&source, 1, "first exact encoded budget");
    let second = document(&source, 2, "second must stay raw-free");
    let encoded_budget = first.encode_stored().unwrap().len();
    let content_budget =
        core_content_bytes(&first.content).unwrap() + core_content_bytes(&second.content).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_budget(
            &[first.event_id.as_uuid(), second.event_id.as_uuid()],
            2,
            CoreEventPageBudget::new(encoded_budget, content_budget),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
}

#[test]
fn strict_core_event_batch_nonzero_encoded_shortfall_skips_all_stored_documents() {
    let temp = tempdir().unwrap();
    let source = source("strict-nonzero-encoded-remainder.jsonl");
    let first = document(&source, 1, "first retained body");
    let second = document(&source, 2, "second raw-only body");
    let first_encoded_bytes = first.encode_stored().unwrap().len();
    let second_encoded_bytes = second.encode_stored().unwrap().len();
    let encoded_budget = first_encoded_bytes + second_encoded_bytes - 1;
    let content_budget =
        core_content_bytes(&first.content).unwrap() + core_content_bytes(&second.content).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(first.clone()).unwrap();
    writer.add_core_record(second.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_budget(
            &[first.event_id.as_uuid(), second.event_id.as_uuid()],
            2,
            CoreEventPageBudget::new(encoded_budget, content_budget),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
}

#[test]
fn strict_core_event_batch_returns_three_exact_records_in_requested_order() {
    let temp = tempdir().unwrap();
    let source = source("strict-exact-order.jsonl");
    let first = document(&source, 1, "first exact body");
    let second = document(&source, 2, "second exact body");
    let third = document(&source, 3, "third exact body");
    let encoded_budget = [&first, &second, &third]
        .into_iter()
        .map(|record| record.encode_stored().unwrap().len())
        .sum();
    let content_budget = [&first, &second, &third]
        .into_iter()
        .map(|record| core_content_bytes(&record.content).unwrap())
        .sum();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in [&second, &third, &first] {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let requested = [
        third.event_id.as_uuid(),
        first.event_id.as_uuid(),
        second.event_id.as_uuid(),
    ];
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let batch = index
        .core_events_by_ids_with_strict_budget(
            &requested,
            requested.len(),
            CoreEventPageBudget::new(encoded_budget, content_budget),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(
        batch
            .items
            .iter()
            .map(|record| &record.core_record)
            .collect::<Vec<_>>(),
        vec![&third, &first, &second]
    );
    assert_eq!(batch.encoded_core_bytes, encoded_budget);
    assert_eq!(batch.content_bytes, content_budget);
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        3
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 3);
}

#[test]
fn strict_core_event_batch_rejects_forged_fast_content_size_after_decode() {
    let temp = tempdir().unwrap();
    let source = source("strict-forged-content-size.jsonl");
    let record = document(&source, 1, "actual content size");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let complete = indexed_document(record.clone());
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != fields.core_content_bytes {
            forged.add_field_value(field, value);
        }
    }
    forged.add_u64(
        fields.core_content_bytes,
        u64::try_from(core_content_bytes(&record.content).unwrap() + 1).unwrap(),
    );
    drop(searcher);

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );
    let pinned = VerifiedIndex::open_pinned(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let result = pinned.core_events_by_ids_with_strict_budget(
        &[record.event_id.as_uuid()],
        1,
        DEFAULT_CORE_EVENT_PAGE_BUDGET,
    );
    assert!(
        matches!(
            result,
            Err(IndexError::InvalidStoredDocumentField("core_content_bytes"))
        ),
        "unexpected strict corruption result: {result:?}"
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 1);

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let mut stream = pinned
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &[record.event_id.as_uuid()],
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap()
        .unwrap();
    let streamed = stream.next().unwrap();
    assert!(matches!(
        streamed,
        Err(IndexError::InvalidStoredDocumentField("core_content_bytes"))
    ));
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 1);
}

#[test]
fn strict_core_event_batch_rejects_forged_fast_encoded_size_before_decode() {
    let temp = tempdir().unwrap();
    let source = source("strict-forged-encoded-size.jsonl");
    let record = document(&source, 1, "actual encoded size");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let (searcher, manifest) = open_unverified_generation(temp.path());
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let complete = indexed_document(record.clone());
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != fields.core_record_encoded_bytes {
            forged.add_field_value(field, value);
        }
    }
    forged.add_u64(
        fields.core_record_encoded_bytes,
        u64::try_from(record.encode_stored().unwrap().len() + 1).unwrap(),
    );
    drop(searcher);

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    publish_unchecked_generation(
        temp.path(),
        &index,
        manifest,
        std::slice::from_ref(&source),
        vec![forged],
    );
    let pinned = VerifiedIndex::open_pinned(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let result = pinned.core_events_by_ids_with_strict_budget(
        &[record.event_id.as_uuid()],
        1,
        DEFAULT_CORE_EVENT_PAGE_BUDGET,
    );
    assert!(
        matches!(
            result,
            Err(IndexError::InvalidStoredDocumentField(
                "core_record_encoded_bytes"
            ))
        ),
        "unexpected strict corruption result: {result:?}"
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let mut stream = pinned
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &[record.event_id.as_uuid()],
            1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap()
        .unwrap();
    let streamed = stream.next().unwrap();
    assert!(matches!(
        streamed,
        Err(IndexError::InvalidStoredDocumentField(
            "core_record_encoded_bytes"
        ))
    ));
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 0);
}

#[test]
fn strict_preflight_preserves_legacy_and_paged_oversized_singleton_behavior() {
    let temp = tempdir().unwrap();
    let source = source("non-strict-singleton-compatibility.jsonl");
    let record = document(&source, 1, "singleton larger than one byte");
    let expected_encoded_bytes = record.encode_stored().unwrap().len();
    let expected_content_bytes = core_content_bytes(&record.content).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_if_bounded(&[record.event_id.as_uuid()], 1, 1)
        .unwrap()
        .is_none());
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 1);

    ctx_history_index_query::reset_stored_core_event_record_materializations();
    ctx_history_index_query::reset_core_record_decodes();
    let page = index
        .core_events_by_ids_with_budget(
            &[record.event_id.as_uuid()],
            1,
            CoreEventPageBudget::new(1, 1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].core_record, record);
    assert_eq!(page.encoded_core_bytes, expected_encoded_bytes);
    assert_eq!(page.content_bytes, expected_content_bytes);
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        1
    );
    assert_eq!(ctx_history_index_query::core_record_decodes(), 1);
}

include!("coordinate_bounds.rs");

include!("query_constraints.rs");
