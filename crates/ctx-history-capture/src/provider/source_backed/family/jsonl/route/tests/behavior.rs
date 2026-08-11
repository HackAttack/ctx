use super::*;
use ctx_history_capture_runtime::{CaptureLifecycleOpenOutcome, CaptureLifecycleSink};

#[test]
fn semantic_retry_restarts_as_replacement_before_emission_and_reports_shared_progress() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("semantic.jsonl");
    fs::write(&transcript, TEST_RECORD).unwrap();
    let observations = Arc::new(Mutex::new(SemanticLifecycleObservations::default()));
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::RetryAppend,
        observations: Arc::clone(&observations),
    };
    let index_root = temp.path().join("index");
    let cold = prepare_semantic_lifecycle_test(&adapter, &root, &index_root, None, &mut Vec::new())
        .unwrap();

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(TEST_RECORD)
        .unwrap();
    let mut publications = Vec::new();
    let replaced = prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &index_root,
        Some(&cold.certificate),
        &mut publications,
    )
    .unwrap();

    assert!(replaced.append.is_none());
    assert_eq!(replaced.certificate.counts().complete_records, 2);
    assert_eq!(
        publications,
        vec![
            (false, TEST_RECORD.len() as u64, 0),
            (false, TEST_RECORD.len() as u64, 0),
        ],
        "replacement retry must emit only replacement pages with shared-owned byte progress"
    );
    let observations = observations.lock().unwrap();
    assert_eq!(
        observations.constructed_modes,
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend,
            JsonlFamilyProjectionMode::Replacement,
        ]
    );
    assert_eq!(observations.preflight_modes, observations.constructed_modes);
    assert!(!observations
        .page_modes
        .contains(&JsonlFamilyProjectionMode::CertifiedAppend));
    assert_eq!(
        observations.finished_modes,
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::Replacement,
        ],
        "the pre-emission append executor must be discarded without finalization"
    );
}

#[test]
fn semantic_classification_cannot_exceed_shared_physical_records() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("semantic.jsonl"), TEST_RECORD).unwrap();
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::Overclassify,
        observations: Arc::new(Mutex::new(SemanticLifecycleObservations::default())),
    };
    let error = match prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &temp.path().join("index"),
        None,
        &mut Vec::new(),
    ) {
        Ok(_) => panic!("overclassified semantic scan unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("semantic classified count exceeds physical records"));
}

#[test]
fn semantic_executor_cannot_finalize_before_shared_terminal_input() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("semantic.jsonl"), TEST_RECORD).unwrap();
    let observations = Arc::new(Mutex::new(SemanticLifecycleObservations::default()));
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::StopBeforeTerminal,
        observations: Arc::clone(&observations),
    };
    let error = match prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &temp.path().join("index"),
        None,
        &mut Vec::new(),
    ) {
        Ok(_) => panic!("unterminated semantic scan unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("semantic scan has no terminal checkpoint"));
    assert_eq!(
        observations.lock().unwrap().finished_modes,
        [JsonlFamilyProjectionMode::Cold],
        "semantic finalization runs, but shared terminal authority still gates certification"
    );
}

#[test]
fn optimized_leaf_execution_keeps_publication_inside_the_shared_family() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = GenerationWriter::open(temp.path().join("index"), test_writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    let mut publications = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |event| {
        if let JsonlLeafOutputEvent::Page {
            append, records, ..
        } = event
        {
            publications.push((append, records.len()));
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let prepared = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup().into(),
        &mut worker,
        &mut output,
    )
    .unwrap();

    assert_eq!(adapter.scans.load(Ordering::SeqCst), 1);
    assert_eq!(publications, vec![(false, 0)]);
    assert!(prepared.append.is_none());
    assert!(matches!(
        prepared.terminal_proof,
        JsonlFamilyTerminalProof::ExactFile { .. }
    ));
    assert_eq!(
        prepared.certificate.parser_revision(),
        adapter.parser_revision()
    );
}

fn optimized_test_certificate(
    adapter: &dyn JsonlFamilyAdapter,
    leaf: &JsonlFamilyLeaf,
    content_digest: [u8; 32],
) -> CertifiedSource {
    let observation = super::leaf::source_observation(leaf.source(), leaf.observation()).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        content_digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 0,
            certified_bytes: TEST_RECORD.len() as u64,
        },
    )
    .unwrap()
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_cross_leaf_binding() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let first = inventory.leaves().first().unwrap();
    let other_source = SourceKey::derive(
        adapter.provider().as_str(),
        TEST_SOURCE_FORMAT,
        TEST_SCHEMA,
        1,
        SourceAnchor::provider_native(
            "terminal-witness-file",
            TypedKey::utf8("other-optimized-leaf").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let other = JsonlFamilyLeaf::bind_observed(
        other_source,
        first.source_path.clone(),
        Arc::clone(&first.authority),
        first.authority_path.clone(),
        first.binding.clone(),
        first.observation.clone(),
    );
    let first_certificate =
        optimized_test_certificate(&adapter, first, Sha256::digest(TEST_RECORD).into());
    let other_certificate =
        optimized_test_certificate(&adapter, &other, Sha256::digest(TEST_RECORD).into());
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, first, &first_certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(other_certificate, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, &other, None, outcome)
        .err()
        .expect("proof from another optimized leaf must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_mismatched_certificate() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let certificate =
        optimized_test_certificate(&adapter, leaf, Sha256::digest(TEST_RECORD).into());
    let mismatched = optimized_test_certificate(&adapter, leaf, [9; 32]);
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, leaf, &certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(mismatched, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, leaf, None, outcome)
        .err()
        .expect("proof from another certificate must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn optimized_leaf_execution_rejects_records_owned_by_another_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: true,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = GenerationWriter::open(temp.path().join("index"), test_writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup().into(),
        &mut worker,
        &mut output,
    )
    .err()
    .expect("wrong-source optimized emission must fail");
    assert!(error
        .to_string()
        .contains("optimized JSONL leaf emitted a record for another source"));
}

fn project_framing_policy_fixture(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    index: &Path,
) -> CertifiedSource {
    let inventory = adapter.discover(root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = GenerationWriter::open(index, test_writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    prepare_leaf(
        adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup().into(),
        &mut worker,
        &mut output,
    )
    .unwrap()
    .certificate
}

fn assert_framing_policy_fixture(
    message: &str,
    record_framing: JsonlRecordFraming,
    includes_terminal_padding: bool,
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let record = format!(r#"{{"message":"{message}"}}"#).into_bytes();
    let mut fixture = record.clone();
    fixture.push(b'\n');
    fixture.extend_from_slice(&[0; 8]);
    fs::write(root.join("framing.jsonl"), &fixture).unwrap();
    let projected = Arc::new(Mutex::new(Vec::new()));
    let adapter = FramingPolicyTestAdapter {
        projected: Arc::clone(&projected),
        record_framing,
    };
    let certificate = project_framing_policy_fixture(&adapter, &root, &temp.path().join("index"));
    let expected = if includes_terminal_padding {
        vec![record, vec![0; 8]]
    } else {
        vec![record]
    };
    assert_eq!(projected.lock().unwrap().as_slice(), expected);
    let expected_count = u64::try_from(expected.len()).unwrap();
    assert_eq!(certificate.counts().complete_records, expected_count);
    assert_eq!(certificate.counts().ignored_records, expected_count);
    assert_eq!(
        certificate.counts().certified_bytes,
        if includes_terminal_padding {
            fixture.len() as u64
        } else {
            (fixture.len() - 8) as u64
        }
    );
}

#[test]
fn adapter_record_framing_defaults_to_ordinary_tail_compatibility() {
    assert_framing_policy_fixture("ordinary", JsonlRecordFraming::ordinary(), false);
}

#[test]
fn adapter_record_framing_can_select_terminal_nul_padding() {
    assert_framing_policy_fixture(
        "terminal",
        JsonlRecordFraming::terminal_nul_padded(crate::MAX_PROVIDER_JSONL_LINE_BYTES),
        true,
    );
}

#[test]
fn generic_projection_streams_record_and_finish_fanout_before_record_65() {
    for finish_only in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("fanout.jsonl"), TEST_RECORD).unwrap();
        let admitted = Arc::new(AtomicUsize::new(0));
        let observed_before_65 = Arc::new(AtomicUsize::new(usize::MAX));
        let adapter = EmissionTestAdapter {
            project_fanout: if finish_only { 0 } else { 129 },
            finish_fanout: if finish_only { 129 } else { 0 },
            admitted: Some(Arc::clone(&admitted)),
            observed_before_65: Some(Arc::clone(&observed_before_65)),
        };
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.leaves().first().unwrap();
        let writer = GenerationWriter::open(temp.path().join("index"), test_writer_options())
            .unwrap()
            .into_writer()
            .unwrap();
        let mut emit = |event| {
            if matches!(event, JsonlLeafOutputEvent::Record { .. }) {
                admitted.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        };
        let mut output = JsonlLeafOutput::new(&mut emit);
        let mut worker = JsonlFamilyWorkerContext::default();
        let prepared = prepare_leaf(
            &adapter,
            leaf,
            None,
            &writer.base_event_identity_lookup().into(),
            &mut worker,
            &mut output,
        )
        .unwrap();

        assert_eq!(admitted.load(Ordering::SeqCst), 129);
        assert_eq!(observed_before_65.load(Ordering::SeqCst), 64);
        assert_eq!(prepared.certificate.counts().indexed_documents, 129);
    }
}

#[test]
fn borrowed_jsonl_worker_policy_honors_default_and_requested_counts() {
    assert_eq!(family_scanner_worker_count_policy(0, None), 0);
    assert_eq!(family_scanner_worker_count_policy(8, None), 8);
    assert_eq!(family_scanner_worker_count_policy(8, Some(4)), 4);
    assert_eq!(family_scanner_worker_count_policy(3, Some(4)), 3);
    assert_eq!(family_scanner_worker_count_policy(8, Some(0)), 1);
    assert_eq!(family_scanner_worker_count_policy(8, Some(usize::MAX)), 8);
}

#[test]
fn certified_append_generation_is_identical_with_one_and_eight_workers() {
    use std::io::Write;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            format!("{{\"message\":\"cold-{index}\"}}\n"),
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;

    let (one_cold, one_cold_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("one"), 1);
    let (eight_cold, eight_cold_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("eight"), 8);
    assert_eq!(
        one_cold_activity,
        JsonlFamilyScannerActivity {
            worker_count: 1,
            sources_started: 8,
            sources_completed: 8,
            peak_active_scanners: 1,
        }
    );
    assert_eq!(eight_cold_activity.worker_count, 8);
    assert_eq!(eight_cold_activity.sources_started, 8);
    assert_eq!(eight_cold_activity.sources_completed, 8);
    assert!(eight_cold_activity.peak_active_scanners >= 4);
    assert!(eight_cold_activity.peak_active_scanners <= 8);
    assert_eq!(one_cold.generation_id, eight_cold.generation_id);
    assert_eq!(
        one_cold.manifest().sources,
        eight_cold.manifest().sources,
        "cold certification must be independent of worker count"
    );

    for index in 0..8 {
        OpenOptions::new()
            .append(true)
            .open(root.join(format!("{index}.jsonl")))
            .unwrap()
            .write_all(format!("{{\"message\":\"append-{index}\"}}\n").as_bytes())
            .unwrap();
    }
    let (one_append, one_append_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("one"), 1);
    let (eight_append, eight_append_activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("eight"), 8);
    assert_eq!(one_append_activity.sources_started, 8);
    assert_eq!(one_append_activity.sources_completed, 8);
    assert_eq!(one_append_activity.peak_active_scanners, 1);
    assert_eq!(eight_append_activity.sources_started, 8);
    assert_eq!(eight_append_activity.sources_completed, 8);
    assert!(eight_append_activity.peak_active_scanners >= 4);
    assert_eq!(one_append.generation_id, eight_append.generation_id);
    assert_eq!(
        one_append.manifest().sources,
        eight_append.manifest().sources,
        "certified append must be independent of worker count"
    );
    assert!(one_append
        .manifest()
        .sources
        .iter()
        .all(|source| source.counts().complete_records == 2));
}

#[test]
fn unchanged_complete_sources_do_not_enter_jsonl_ingestion_tasks() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    for source_index in 0..4 {
        fs::write(
            root.join(format!("{source_index}.jsonl")),
            format!("{{\"message\":\"cold-{source_index}\"}}\n"),
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;

    let (cold, cold_activity) = capture_parallel_test_generation(&adapter, &root, &index, 4);
    assert_eq!(cold_activity.sources_started, 4);
    let (unchanged, unchanged_activity) =
        capture_parallel_test_generation(&adapter, &root, &index, 4);

    assert_eq!(unchanged.generation_id, cold.generation_id);
    assert_eq!(unchanged.manifest().sources, cold.manifest().sources);
    assert_eq!(unchanged_activity, JsonlFamilyScannerActivity::default());
}

#[test]
fn unchanged_terminal_proof_allows_growth_before_terminal_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("growing.jsonl");
    fs::write(&source_path, TEST_RECORD).unwrap();
    let adapter = ParallelTestAdapter;
    let (cold, _) = capture_parallel_test_generation(&adapter, &root, &index, 1);

    set_before_jsonl_terminal_physical_revalidation_hook(root.clone(), move || {
        OpenOptions::new()
            .append(true)
            .open(source_path)
            .unwrap()
            .write_all(TEST_RECORD)
            .unwrap();
    });

    let (unchanged, activity) =
        capture_parallel_test_generation_with_terminal_revalidation(&adapter, &root, &index, 1)
            .unwrap();

    assert_eq!(unchanged.generation_id, cold.generation_id);
    assert_eq!(activity, JsonlFamilyScannerActivity::default());

    let (resumed, activity) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(activity.sources_started, 1);
    assert_eq!(activity.sources_completed, 1);
    assert_eq!(resumed.manifest().sources[0].counts().complete_records, 2);
}

#[test]
fn unchanged_terminal_proof_fails_closed_on_prepublication_source_races() {
    let append_adapter = ParallelTestAdapter;
    let replacement_adapter = ReplacementParallelTestAdapter;
    for (proof_kind, adapter) in [
        ("frozen-prefix", &append_adapter as &dyn JsonlFamilyAdapter),
        (
            "exact-file",
            &replacement_adapter as &dyn JsonlFamilyAdapter,
        ),
    ] {
        for race in ["mutation", "replacement", "deletion"] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = temp.path().join("sessions");
            let index = temp.path().join("index");
            fs::create_dir_all(&root).unwrap();
            let source_path = root.join("racing.jsonl");
            fs::write(&source_path, TEST_RECORD).unwrap();
            let cold = capture_parallel_test_generation(adapter, &root, &index, 1).0;

            let displaced = temp.path().join("displaced.jsonl");
            let replacement = temp.path().join("replacement.jsonl");
            if race == "replacement" {
                fs::write(&replacement, TEST_RECORD).unwrap();
            }
            let hook_ran = Arc::new(AtomicBool::new(false));
            let hook_observation = Arc::clone(&hook_ran);
            let hook_source = source_path.clone();
            set_before_jsonl_terminal_physical_revalidation_hook(root.clone(), move || {
                match race {
                    "mutation" => {
                        fs::write(&hook_source, b"{\"message\":\"after!\"}\n").unwrap();
                    }
                    "replacement" => {
                        fs::rename(&hook_source, displaced).unwrap();
                        fs::rename(replacement, &hook_source).unwrap();
                    }
                    "deletion" => fs::remove_file(&hook_source).unwrap(),
                    _ => unreachable!(),
                }
                hook_observation.store(true, Ordering::SeqCst);
            });

            let error = capture_parallel_test_generation_with_terminal_revalidation(
                adapter, &root, &index, 1,
            )
            .unwrap_err();

            assert!(hook_ran.load(Ordering::SeqCst), "{proof_kind} {race}");
            assert!(
                matches!(error, IndexError::CompleteInventoryInvalidated { .. }),
                "{proof_kind} {race} produced {error:?}"
            );
            assert_eq!(
                jsonl_family_scanner_activity(),
                JsonlFamilyScannerActivity::default(),
                "{proof_kind} {race} did not take unchanged admission"
            );
            assert_eq!(
                VerifiedIndex::open(&index).unwrap().generation_id(),
                cold.generation_id,
                "{proof_kind} {race} became visible"
            );
        }
    }
}

#[test]
fn event_identity_revision_forces_replacement_with_core_base_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("identity.jsonl"), b"{\"message\":\"stable\"}\n").unwrap();

    let cold = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Cold,
    };
    let (cold_receipt, _) = capture_parallel_test_generation(&cold, &root, &index, 1);

    let upgraded = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v2",
        expected_mode: JsonlFamilyProjectionMode::Replacement,
    };
    let (upgraded_receipt, _) = capture_parallel_test_generation(&upgraded, &root, &index, 1);

    assert_ne!(cold_receipt.generation_id, upgraded_receipt.generation_id);
    let checkpoint = upgraded_receipt.manifest().sources[0]
        .frontier()
        .unwrap()
        .checkpoint();
    assert_eq!(
        FamilyCheckpoint::decode_frontier_key(checkpoint)
            .unwrap()
            .event_identity_revision,
        "content-occurrence-v2"
    );
}

#[test]
fn parser_revision_forces_unchanged_source_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("parser.jsonl"), b"{\"message\":\"stable\"}\n").unwrap();

    let cold = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v1",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Cold,
    };
    let (cold_receipt, _) = capture_parallel_test_generation(&cold, &root, &index, 1);

    let upgraded = IdentityRevisionTestAdapter {
        parser_revision: "identity-revision-test-parser-v2",
        revision: "content-occurrence-v1",
        expected_mode: JsonlFamilyProjectionMode::Replacement,
    };
    let (upgraded_receipt, _) = capture_parallel_test_generation(&upgraded, &root, &index, 1);

    assert_ne!(cold_receipt.generation_id, upgraded_receipt.generation_id);
    assert_eq!(
        upgraded_receipt.manifest().sources[0].parser_revision(),
        "identity-revision-test-parser-v2"
    );
}

#[test]
fn production_jsonl_scheduler_projects_multiple_sources_concurrently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        fs::write(
            root.join(format!("{index}.jsonl")),
            b"{\"message\":\"parallel\"}\n",
        )
        .unwrap();
    }
    let adapter = ParallelTestAdapter;
    let resident = Mutex::new(FamilyResident::default());
    let mut writer =
        match IndexCaptureLifecycle::open(&temp.path().join("index"), test_writer_options())
            .unwrap()
        {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
                panic!("test lifecycle unexpectedly requires recovery")
            }
        };
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut sink = SourceBackedGenerationSink {
        core_record_preparer: writer.core_preparation(),
        lifecycle: &mut writer,
        owners: &mut owners,
        complete_inventories: &mut complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        base_route_control: None,
        resources: SourceBackedRouteResources::production(4),
        logical_source_failures: &mut logical_source_failures,
        record_rejections: &mut record_rejections,
        applied_removals: &mut Vec::new(),
        record_progress: None,
        current_source_progress: None,
        last_progress_session_id: None,
    };

    with_family_scanner_workers(4, || {
        capture(&adapter, &root, &resident, &mut sink).unwrap();
    });

    assert_eq!(
        jsonl_family_scanner_activity(),
        JsonlFamilyScannerActivity {
            worker_count: 4,
            sources_started: 8,
            sources_completed: 8,
            peak_active_scanners: 4,
        },
        "the production JSONL route must keep all four selected scanners active"
    );
    assert_eq!(resident.lock().unwrap().terminal_sources.len(), 8);
}

#[test]
fn dependency_phases_bar_later_jsonl_scans_without_serializing_each_phase() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for phase in ["first", "second"] {
        for index in 0..4 {
            fs::write(
                root.join(format!("{phase}-{index}.jsonl")),
                b"{\"message\":\"phased\"}\n",
            )
            .unwrap();
        }
    }
    let completed_first_phase = Arc::new(AtomicUsize::new(0));
    let second_phase_started_early = Arc::new(AtomicBool::new(false));
    let adapter = PhasedTestAdapter {
        completed_first_phase: Arc::clone(&completed_first_phase),
        second_phase_started_early: Arc::clone(&second_phase_started_early),
    };

    let (_, activity) =
        capture_parallel_test_generation(&adapter, &root, &temp.path().join("index"), 4);

    assert_eq!(completed_first_phase.load(Ordering::SeqCst), 4);
    assert!(!second_phase_started_early.load(Ordering::SeqCst));
    assert_eq!(activity.sources_started, 8);
    assert_eq!(activity.sources_completed, 8);
    assert_eq!(activity.peak_active_scanners, 4);
}

#[test]
fn partitioned_component_balances_hooks_and_parallelizes_parent_first_frontiers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 7, 0, 0);
    write_scheduler_test_leaf(&root, 7, 1, 0);
    write_scheduler_test_leaf(&root, 7, 1, 1);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: Some((7, 1, Arc::new(std::sync::Barrier::new(2)))),
        events: Arc::clone(&events),
    };

    let activity =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 4).unwrap();
    let events = events.lock().unwrap().clone();
    let hooks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(7),
            SchedulerStateEvent::Finish(7)
        ]
    );
    let project_order = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project { leaf, .. } => Some(*leaf),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    let parent = SchedulerLeafState {
        partition: 7,
        phase: 0,
        ordinal: 0,
    };
    assert_eq!(project_order.first(), Some(&parent));

    let mut projects = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            } => Some((
                *leaf,
                *full_probes_before,
                *full_probes_after,
                *event_time_entries_before,
                *event_time_entries_after,
            )),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 3);
    assert_eq!((projects[0].1, projects[0].2), (0, 1));
    assert_eq!(
        projects
            .iter()
            .map(|(_, before, after, _, _)| after.saturating_sub(*before))
            .sum::<usize>(),
        2,
        "the parent lane should reuse its repository certificate while the parallel sibling lane probes once"
    );
    assert!(projects
        .iter()
        .all(|(_, _, _, event_entries_before, event_entries_after)| (
            *event_entries_before,
            *event_entries_after
        ) == (0, 1)));
    assert_eq!(activity.worker_count, 3);
    assert_eq!(activity.sources_started, 3);
    assert_eq!(activity.sources_completed, 3);
    assert_eq!(activity.peak_active_scanners, 2);
}

#[test]
fn partitioned_generation_is_identical_with_one_and_three_workers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 7, 0, 0);
    write_scheduler_test_leaf(&root, 7, 1, 0);
    write_scheduler_test_leaf(&root, 7, 1, 1);

    let repository = scheduler_test_repository(temp.path());
    let one = SchedulerStateTestAdapter {
        repository: repository.clone(),
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let three = SchedulerStateTestAdapter {
        repository,
        attributed_partitions: vec![7],
        failing_leaf: None,
        parallel_frontier: Some((7, 1, Arc::new(std::sync::Barrier::new(2)))),
        events: Arc::new(Mutex::new(Vec::new())),
    };

    let (one_receipt, one_activity) =
        capture_parallel_test_generation(&one, &root, &temp.path().join("one"), 1);
    let (three_receipt, three_activity) =
        capture_parallel_test_generation(&three, &root, &temp.path().join("three"), 3);

    assert_eq!(one_activity.peak_active_scanners, 1);
    assert!(three_activity.peak_active_scanners >= 2);
    assert_eq!(one_receipt.generation_id, three_receipt.generation_id);
    assert_eq!(
        one_receipt.manifest().sources,
        three_receipt.manifest().sources
    );
}

#[test]
fn partition_scan_failure_finishes_every_begun_component() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 2, 0, 0);
    write_scheduler_test_leaf(&root, 3, 0, 0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: Vec::new(),
        failing_leaf: Some(SchedulerLeafState {
            partition: 3,
            phase: 0,
            ordinal: 0,
        }),
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    let error =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap_err();
    assert!(error
        .detail
        .contains("scheduler test requested scan failure"));
    let hooks = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(2),
            SchedulerStateEvent::Begin(3),
            SchedulerStateEvent::Finish(3),
            SchedulerStateEvent::Finish(2),
        ],
        "every begun component must finish exactly once even when its wave fails"
    );
}

#[test]
fn partition_lifecycle_ids_are_separate_from_frontier_worker_lanes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_scheduler_test_leaf(&root, 2, 0, 0);
    write_scheduler_test_leaf(&root, 3, 0, 0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![2, 3],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap();
    let events = events.lock().unwrap().clone();
    let hooks = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        hooks,
        [
            SchedulerStateEvent::Begin(2),
            SchedulerStateEvent::Begin(3),
            SchedulerStateEvent::Finish(3),
            SchedulerStateEvent::Finish(2),
        ],
        "dense lifecycle IDs must continue to drive deterministic component hooks"
    );

    let mut projects = events
        .iter()
        .filter_map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                ..
            } => Some((*leaf, *full_probes_before, *full_probes_after)),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 2);
    assert_eq!((projects[0].1, projects[0].2), (0, 1));
    assert_eq!((projects[1].1, projects[1].2), (0, 1));
}

#[test]
fn partition_waves_admit_largest_components_first() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for partition in 0..17 {
        write_scheduler_test_leaf(&root, partition, 0, 0);
    }
    fs::write(
        root.join("partition-16-phase-0-leaf-0.jsonl"),
        b"{\"message\":\"large scheduler component\"}\n".repeat(128),
    )
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: Vec::new(),
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    };

    run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 4).unwrap();
    let events = events.lock().unwrap();
    let first_hook = events.iter().find(|event| {
        matches!(
            event,
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
        )
    });
    assert_eq!(first_hook, Some(&SchedulerStateEvent::Begin(16)));
}

#[test]
fn partition_logical_cache_lanes_are_fixed_and_clear_source_semantic_state() {
    for workers in [1, 4, 16] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        for partition in 0..32 {
            write_scheduler_test_leaf(&root, partition, 0, 0);
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let adapter = SchedulerStateTestAdapter {
            repository: scheduler_test_repository(temp.path()),
            attributed_partitions: (0..32).collect(),
            failing_leaf: None,
            parallel_frontier: None,
            events: Arc::clone(&events),
        };

        let activity =
            run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), workers)
                .unwrap();
        let events = events.lock().unwrap().clone();
        let hooks = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(hooks.len(), 64);
        let mut begun_partitions = Vec::new();
        for wave in hooks.chunks(32) {
            let begun = wave[..16]
                .iter()
                .map(|event| match event {
                    SchedulerStateEvent::Begin(partition) => *partition,
                    _ => panic!("partition wave did not begin before finishing"),
                })
                .collect::<Vec<_>>();
            let finished = wave[16..]
                .iter()
                .map(|event| match event {
                    SchedulerStateEvent::Finish(partition) => *partition,
                    _ => panic!("partition wave did not finish in its closing half"),
                })
                .collect::<Vec<_>>();
            assert_eq!(finished, begun.iter().rev().copied().collect::<Vec<_>>());
            begun_partitions.extend(begun);
        }
        begun_partitions.sort_unstable();
        assert_eq!(begun_partitions, (0_u64..32).collect::<Vec<_>>());

        let mut projects = events
            .iter()
            .filter_map(|event| match event {
                SchedulerStateEvent::Project {
                    leaf,
                    full_probes_before,
                    full_probes_after,
                    event_time_entries_before,
                    event_time_entries_after,
                } => Some((
                    *leaf,
                    *full_probes_before,
                    *full_probes_after,
                    *event_time_entries_before,
                    *event_time_entries_after,
                )),
                SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => None,
            })
            .collect::<Vec<_>>();
        projects.sort_by_key(|project| project.0);
        assert_eq!(projects.len(), 32);
        let full_probes = projects
            .iter()
            .map(|(_, before, after, _, _)| after.saturating_sub(*before))
            .sum::<usize>();
        assert_eq!(
            full_probes, 16,
            "same-repository components must reuse fixed logical cache lanes independently of physical workers"
        );
        for (leaf, _, _, event_entries_before, event_entries_after) in &projects {
            assert_eq!(
                *event_entries_before, 0,
                "component {} leaked source-semantic event-time state on its shared cache lane",
                leaf.partition
            );
            assert_eq!(*event_entries_after, 1);
        }
        assert_eq!(activity.worker_count, workers);
        assert_eq!(activity.sources_started, 32);
        assert_eq!(activity.sources_completed, 32);
    }
}

#[test]
fn unpartitioned_defaults_keep_persistent_phase_worker_contexts() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for phase in 0..=1 {
        for ordinal in 0..=1 {
            write_scheduler_test_leaf(&root, 0, phase, ordinal);
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let adapter = UnpartitionedSchedulerStateTestAdapter(SchedulerStateTestAdapter {
        repository: scheduler_test_repository(temp.path()),
        attributed_partitions: vec![0],
        failing_leaf: None,
        parallel_frontier: None,
        events: Arc::clone(&events),
    });

    let activity =
        run_scheduler_test_capture(&adapter, &root, &temp.path().join("index"), 2).unwrap();
    let mut projects = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| match event {
            SchedulerStateEvent::Project {
                leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            } => (
                *leaf,
                *full_probes_before,
                *full_probes_after,
                *event_time_entries_before,
                *event_time_entries_after,
            ),
            SchedulerStateEvent::Begin(_) | SchedulerStateEvent::Finish(_) => {
                panic!("unpartitioned defaults must not call partition hooks")
            }
        })
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.0);
    assert_eq!(projects.len(), 4);
    for (leaf, probes_before, probes_after, event_entries_before, event_entries_after) in projects {
        assert_eq!(probes_before, usize::from(leaf.phase == 1));
        assert_eq!(probes_after, 1);
        assert_eq!(event_entries_before, 0);
        assert_eq!(event_entries_after, 1);
    }
    assert_eq!(activity.worker_count, 2);
    assert_eq!(activity.sources_started, 4);
    assert_eq!(activity.sources_completed, 4);
    assert_eq!(activity.peak_active_scanners, 2);
}

#[test]
fn serial_and_parallel_jsonl_emission_preserve_resource_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for workers in [1, 4] {
        let root = temp.path().join(format!("sessions-{workers}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..workers {
            fs::write(
                root.join(format!("{index}.jsonl")),
                b"{\"message\":\"bounded\"}\n",
            )
            .unwrap();
        }
        let resident = Mutex::new(FamilyResident::default());
        let mut writer = match IndexCaptureLifecycle::open(
            &temp.path().join(format!("index-{workers}")),
            test_writer_options(),
        )
        .unwrap()
        {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
                panic!("test lifecycle unexpectedly requires recovery")
            }
        };
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let mut sink = SourceBackedGenerationSink {
            core_record_preparer: writer.core_preparation(),
            lifecycle: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventories,
            route_index: 0,
            route_identity: test_route_identity(),
            base_route_control: None,
            resources: SourceBackedRouteResources::for_test(workers, 1, u64::MAX),
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            applied_removals: &mut Vec::new(),
            record_progress: None,
            current_source_progress: None,
            last_progress_session_id: None,
        };

        let error = with_family_scanner_workers(workers, || {
            capture(
                &EmissionTestAdapter::ordinary(),
                &root,
                &resident,
                &mut sink,
            )
            .unwrap_err()
        });
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    }
}

#[test]
fn jsonl_terminal_drift_and_io_failures_keep_distinct_route_kinds() {
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SourceChangedDuringCapture),
        Some(SourceBackedRouteErrorKind::SourceChanged)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(5))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(24))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SystemInvariant("broken route")),
        Some(SourceBackedRouteErrorKind::Internal)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::WorkerPanicked("broken worker")),
        Some(SourceBackedRouteErrorKind::Internal)
    );
    assert_eq!(
        route_scan(
            &TestAdapter,
            CaptureError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
        )
        .kind,
        SourceBackedRouteErrorKind::SourceChanged
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_observes_live_tree() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, b"{\"message\":\"before\"}\n").unwrap();
    let adapter = TestAdapter;

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(&first, b"{\"message\":\"changed between callbacks\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(root.join("new.jsonl"), b"{\"message\":\"late leaf\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_admitted_leaf_symlink_race() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let selected = root.join("first.jsonl");
    fs::write(&selected, TEST_RECORD).unwrap();
    let outside = temp.path().join("outside.jsonl");
    fs::write(&outside, b"{\"message\":\"outside must not be read\"}\n").unwrap();
    let adapter = TerminalLeafSwapTestAdapter {
        selected,
        outside,
        enabled: AtomicBool::new(false),
        swapped: AtomicBool::new(false),
    };
    let (resident, inventory) = expected_state(&adapter, &root);
    let resident = Mutex::new(resident);
    adapter.enabled.store(true, Ordering::SeqCst);

    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap(),
        "an admitted transcript that becomes a symlink must fail terminal membership"
    );
    assert!(adapter.swapped.load(Ordering::SeqCst));
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_accepts_proven_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, TEST_RECORD).unwrap();
    let adapter = TestAdapter;
    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));

    OpenOptions::new()
        .append(true)
        .open(&first)
        .unwrap()
        .write_all(b"{\"message\":\"next refresh\"}\n")
        .unwrap();
    assert!(revalidate_complete_inventory(&adapter, &root, &resident, &inventory,).unwrap());
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), b"{\"message\":\"kept\"}\n").unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, b"{\"message\":\"old\"}\n").unwrap();
    let adapter = TestAdapter;
    let before = adapter.discover(&root).unwrap();
    let deleted_source = before
        .leaves()
        .iter()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, b"{\"message\":\"reappeared\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[test]
fn active_source_family_contract_jsonl_frozen_multi_root_defers_new_leaves() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_root = temp.path().join("sessions");
    let second_root = temp.path().join("archived_sessions");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let retained = first_root.join("first.jsonl");
    fs::write(&retained, TEST_RECORD).unwrap();
    fs::write(second_root.join("archived.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![first_root.clone(), second_root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::write(second_root.join("late.jsonl"), TEST_RECORD).unwrap();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::remove_file(retained).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,)
            .unwrap_or(false)
    );
}

#[test]
fn active_source_family_contract_jsonl_frozen_root_replacement_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    let moved = temp.path().join("moved-sessions");
    fs::rename(&root, &moved).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_noop_is_metadata_only_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root,
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    reset_jsonl_prefix_hash_bytes();
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory).unwrap()
    );
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(jsonl_prefix_hash_bytes(), 0);
}

#[test]
fn active_source_family_contract_jsonl_frozen_rejects_root_swap_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root: root.clone(),
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    fs::OpenOptions::new()
        .append(true)
        .open(root.join("first.jsonl"))
        .unwrap()
        .write_all(b"{\"message\":\"appended\"}\n")
        .unwrap();
    let moved = temp.path().join("moved-sessions");
    let swap_root = root.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        fs::rename(&swap_root, moved).unwrap();
        fs::create_dir(&swap_root).unwrap();
        fs::write(swap_root.join("first.jsonl"), TEST_RECORD).unwrap();
        worker_barrier.wait();
    });
    set_after_jsonl_prefix_hash_hook(move || {
        barrier.wait();
        barrier.wait();
    });

    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
    );
    worker.join().unwrap();
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
}

#[test]
fn active_source_family_contract_jsonl_frozen_inventory_rejects_deleted_source_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), TEST_RECORD).unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let before = adapter.discover(&selection_root).unwrap();
    let deleted_source = before
        .leaves()
        .iter()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &selection_root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    resident.owned_sources.insert(
        deleted_source.exact_descriptor_digest(),
        deleted_source.clone(),
    );
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, TEST_RECORD).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );
}
