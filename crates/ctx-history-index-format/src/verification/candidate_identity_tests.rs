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

    let error = verify_publication_candidate(&candidate, &manifest(17), Some(&base)).unwrap_err();

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
