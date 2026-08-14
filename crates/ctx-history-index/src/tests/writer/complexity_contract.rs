use super::*;

const RETAINED_N: u64 = 32;
const RETAINED_2N: u64 = RETAINED_N * 2;

#[derive(Debug, PartialEq, Eq)]
struct PublicationWork {
    checksum_walks: usize,
    logical_passes: usize,
    identity_terms: usize,
    identity_documents: usize,
    projection_documents: usize,
    lineage_decodes: usize,
    lineage_spills: usize,
    complete_session_id_traversals: usize,
    hashed_artifact_bytes: u64,
    writer_constructions: usize,
}

impl PublicationWork {
    fn capture(writer_constructions: usize) -> Self {
        let (checksum_walks, logical_passes) = crate::publication::verification_activity();
        let (identity_terms, identity_documents) =
            crate::publication::candidate_identity_verification_activity();
        let (lineage_decodes, lineage_spills) =
            crate::publication::candidate_lineage_verification_activity();
        Self {
            checksum_walks,
            logical_passes,
            identity_terms,
            identity_documents,
            projection_documents: crate::publication::candidate_projection_verification_activity(),
            lineage_decodes,
            lineage_spills,
            complete_session_id_traversals: crate::publication::complete_session_id_traversals(),
            hashed_artifact_bytes: crate::publication::hashed_artifact_bytes(),
            writer_constructions,
        }
    }
}

struct RetainedFixture {
    root: TempDir,
    source: SourceKey,
    inventory: CertifiedSourceInventory,
}

fn retained_fixture(retained_documents: u64) -> RetainedFixture {
    let root = tempdir().unwrap();
    let source = source(&format!("complexity-{retained_documents}.jsonl"));
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    let mut writer = GenerationWriter::open(root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=retained_documents {
        writer
            .add_core_record(document(&source, sequence, "retained body"))
            .unwrap();
    }
    writer
        .certify_source(appendable_certificate(
            &source,
            1,
            retained_documents,
            retained_documents * 10,
        ))
        .unwrap();
    writer.commit(|_| true).unwrap();
    RetainedFixture {
        root,
        source,
        inventory,
    }
}

fn measure_exact_noop(retained_documents: u64) -> PublicationWork {
    let fixture = retained_fixture(retained_documents);
    let mut writer = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&writer.index_writer_constructions);
    writer
        .certify_complete_inventory(fixture.inventory.clone())
        .unwrap();
    stage_exact_replay(&mut writer, &fixture.source);

    crate::publication::reset_verification_activity();
    writer
        .commit_with_complete_inventory_revalidation(
            |_| true,
            |current| current == &fixture.inventory,
        )
        .unwrap();
    PublicationWork::capture(constructions.load(Ordering::SeqCst))
}

fn measure_one_record_append(retained_documents: u64) -> PublicationWork {
    let fixture = retained_fixture(retained_documents);
    let mut writer = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&writer.index_writer_constructions);
    let base = writer
        .begin_source_append(fixture.source.clone())
        .unwrap()
        .clone();
    writer
        .add_core_record(document(
            &fixture.source,
            retained_documents + 1,
            "one appended body",
        ))
        .unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    &fixture.source,
                    2,
                    retained_documents + 1,
                    (retained_documents + 1) * 10,
                ),
                retained_documents * 10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();

    crate::publication::reset_verification_activity();
    writer.commit(|_| true).unwrap();
    PublicationWork::capture(constructions.load(Ordering::SeqCst))
}

#[test]
fn exact_noop_work_is_zero_for_n_and_2n_retained_documents() {
    let n = measure_exact_noop(RETAINED_N);
    let two_n = measure_exact_noop(RETAINED_2N);

    assert_eq!(n, two_n, "W_noop(2N) must equal W_noop(N)");
    assert_eq!(
        n,
        PublicationWork {
            checksum_walks: 0,
            logical_passes: 0,
            identity_terms: 0,
            identity_documents: 0,
            projection_documents: 0,
            lineage_decodes: 0,
            lineage_spills: 0,
            complete_session_id_traversals: 0,
            hashed_artifact_bytes: 0,
            writer_constructions: 0,
        },
        "an exact replay must do no publication or verification work"
    );
}

#[test]
fn one_record_append_verification_work_is_independent_of_retained_corpus_size() {
    let n = measure_one_record_append(RETAINED_N);
    let two_n = measure_one_record_append(RETAINED_2N);

    assert_eq!(
        n.logical_passes, 0,
        "an append must not run a full logical pass"
    );
    assert_eq!(two_n.logical_passes, 0, "W_full_pass(2N) must remain zero");
    assert_eq!(n.checksum_walks, 1);
    assert_eq!(two_n.checksum_walks, n.checksum_walks);
    assert_eq!(n.identity_terms, 1);
    assert_eq!(n.identity_documents, 1);
    assert_eq!(n.projection_documents, 0);
    assert_eq!(n.lineage_decodes, 0);
    assert_eq!(n.lineage_spills, 0);
    assert_eq!(n.complete_session_id_traversals, 0);
    assert_eq!(n.writer_constructions, 1);
    assert_eq!(two_n.identity_terms, n.identity_terms);
    assert_eq!(two_n.identity_documents, n.identity_documents);
    assert_eq!(two_n.projection_documents, n.projection_documents);
    assert_eq!(two_n.lineage_decodes, n.lineage_decodes);
    assert_eq!(two_n.lineage_spills, n.lineage_spills);
    assert_eq!(
        two_n.complete_session_id_traversals,
        n.complete_session_id_traversals
    );
    assert_eq!(two_n.writer_constructions, n.writer_constructions);
}

#[test]
fn cold_writer_publication_avoids_an_exhaustive_logical_pass() {
    let root = tempdir().unwrap();
    let source = source("complexity-cold.jsonl");
    let mut writer = GenerationWriter::open(root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=RETAINED_N {
        writer
            .add_core_record(document(&source, sequence, "cold body"))
            .unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, RETAINED_N))
        .unwrap();

    crate::publication::reset_verification_activity();
    writer.commit(|_| true).unwrap();
    let (_, logical_passes) = crate::publication::verification_activity();
    assert_eq!(
        logical_passes, 0,
        "writer-produced proof must replace exhaustive cold logical replay"
    );
}

#[test]
fn session_registry_budget_counts_unique_changes_not_noops_or_same_session_appends() {
    use crate::writer_options::CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;

    assert!(
        WriterOptions::default().memory_bytes / CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES
            >= 9_000,
        "the default writer budget must admit the known 9K-session corpus"
    );
    assert!(
        std::mem::size_of::<(Uuid, PreparedSessionIdentityFacts)>() + std::mem::size_of::<Uuid>()
            < CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES,
        "the conservative charge must cover registry payload plus route undo UUID"
    );

    let fixture = retained_fixture(1);
    let initial_generation = VerifiedIndex::open(fixture.root.path())
        .unwrap()
        .generation_id()
        .to_owned();

    let mut noop = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    noop.changed_session_registry_memory_bytes = 0;
    noop.certify_complete_inventory(fixture.inventory.clone())
        .unwrap();
    stage_exact_replay(&mut noop, &fixture.source);
    let noop_receipt = noop
        .commit_with_complete_inventory_revalidation(
            |_| true,
            |current| current == &fixture.inventory,
        )
        .unwrap();
    assert_eq!(noop_receipt.generation_id, initial_generation);

    let mut append = GenerationWriter::open(fixture.root.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    append.changed_session_registry_memory_bytes = CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES;
    let base = append
        .begin_source_append(fixture.source.clone())
        .unwrap()
        .clone();
    append
        .add_core_record(document(&fixture.source, 2, "first same-session append"))
        .unwrap();
    append
        .add_core_record(document(&fixture.source, 3, "second same-session append"))
        .unwrap();
    assert_eq!(append.changed_sessions.len(), 1);

    let error = append
        .add_core_record(document_for_session(
            &fixture.source,
            "second-session",
            4,
            "over budget",
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::ChangedSessionRegistryMemoryLimitExceeded {
            attempted_entries: 2,
            required_bytes: 2048,
            maximum_bytes: 1024,
            maximum_entries: 1,
        }
    ));
    assert_eq!(append.changed_sessions.len(), 1);

    let frontier = base.frontier().unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&fixture.source, 2, 3, 30),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .unwrap(),
        )
        .unwrap();
    append.commit(|_| true).unwrap();

    let published = VerifiedIndex::open(fixture.root.path()).unwrap();
    assert_eq!(published.manifest().indexed_documents, 3);
    assert_eq!(published.count_term("same").unwrap(), 2);
    assert_eq!(published.count_term("budget").unwrap(), 0);
}
