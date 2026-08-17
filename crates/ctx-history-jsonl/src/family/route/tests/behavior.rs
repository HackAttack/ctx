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
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
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
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
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

#[test]
fn single_leaf_serial_jsonl_page_accounts_sessions_messages_and_tool_calls() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("progress.jsonl"), PROGRESS_TEST_RECORDS).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
        emit_progress_records: true,
    };
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = match IndexCaptureLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("serial progress test lifecycle unexpectedly requires recovery")
        }
    };
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut applied_removals = Vec::new();
    let mut history_progress = AttemptHistoryProgress::default();
    let mut report_progress = |delta| {
        history_progress.advance(&delta);
        Ok(())
    };
    let mut sink = SourceBackedGenerationSink {
        core_record_preparer: writer.core_preparation(),
        lifecycle: &mut writer,
        owners: &mut owners,
        complete_inventories: &mut complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        base_route_control: None,
        resources: SourceBackedRouteResources::production(1),
        logical_source_failures: &mut logical_source_failures,
        record_rejections: &mut record_rejections,
        applied_removals: &mut applied_removals,
        record_progress: Some(&mut report_progress),
        current_source_progress: None,
        last_progress_session_id: None,
        exact_scan_total_bytes: None,
        exact_scan_accounting_enabled: false,
    };

    with_family_scanner_workers(1, || {
        capture(&adapter, &root, &resident, &mut sink).unwrap();
    });
    drop(sink);

    assert_eq!(
        history_progress.snapshot(),
        ctx_history_capture_model::AttemptHistoryProgressSnapshot {
            processed_sessions: 1,
            processed_messages: 2,
            processed_tool_calls: 1,
            processed_bytes: PROGRESS_TEST_RECORDS.len() as u64,
        },
        "the true one-leaf serial page path must preserve Core-record progress semantics"
    );
}

fn optimized_test_certificate(
    adapter: &JsonlFamilyAdapterObject,
    leaf: &JsonlFamilyLeaf,
    content_digest: [u8; 32],
) -> CertifiedSource {
    let observation =
        super::scanner::source_observation::<CaptureError>(leaf.source(), leaf.observation())
            .unwrap();
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
        emit_progress_records: false,
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
        emit_progress_records: false,
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
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
    )
    .err()
    .expect("wrong-source optimized emission must fail");
    assert!(error
        .to_string()
        .contains("optimized JSONL leaf emitted a record for another source"));
}

fn project_framing_policy_fixture(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index: &Path,
) -> CertifiedSource {
    let inventory = adapter.discover(root).unwrap();
    let leaf = inventory.leaves().first().unwrap();
    let writer = match TestLifecycle::open(index, ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    prepare_leaf(
        adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
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
        JsonlRecordFraming::terminal_nul_padded(MAX_PROVIDER_JSONL_LINE_BYTES),
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
        let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
            CaptureLifecycleOpenOutcome::Ready(writer) => writer,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
        };
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
            &writer.base_event_identity_lookup(),
            &mut worker,
            &mut output,
            true,
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
fn append_only_terminal_growth_commits_admitted_suffix_and_successor_drains_later_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("actively-growing.jsonl");
    fs::write(&source_path, TEST_RECORD).unwrap();
    let adapter = DirectAppendTestAdapter::default();
    let (cold, _) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(cold.manifest().sources[0].counts().complete_records, 1);

    OpenOptions::new()
        .append(true)
        .open(&source_path)
        .unwrap()
        .write_all(TEST_RECORD)
        .unwrap();
    let terminal_append_path = source_path.clone();
    set_before_jsonl_terminal_physical_revalidation_hook(root.clone(), move || {
        OpenOptions::new()
            .append(true)
            .open(terminal_append_path)
            .unwrap()
            .write_all(TEST_RECORD)
            .unwrap();
    });

    let active_prefix_hash = track_jsonl_prefix_hash_bytes(source_path.clone());
    let (active, _) =
        capture_parallel_test_generation_with_terminal_revalidation(&adapter, &root, &index, 1)
            .unwrap();
    assert_eq!(active_prefix_hash.bytes(), 0);
    assert_eq!(active.manifest().sources[0].counts().complete_records, 2);
    let active_observation = *adapter.observations.lock().unwrap().last().unwrap();
    assert_eq!(
        active_observation,
        DirectAppendPassObservation {
            mode: JsonlFamilyProjectionMode::CertifiedAppend,
            direct_append: true,
            preflight_bytes: TEST_RECORD.len() as u64,
            projection_bytes: TEST_RECORD.len() as u64,
            projected_records: 1,
        }
    );

    let successor_prefix_hash = track_jsonl_prefix_hash_bytes(source_path);
    let (successor, _) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(successor_prefix_hash.bytes(), 0);
    assert_eq!(successor.manifest().sources[0].counts().complete_records, 3);
    let successor_observation = *adapter.observations.lock().unwrap().last().unwrap();
    assert_eq!(
        successor_observation,
        DirectAppendPassObservation {
            mode: JsonlFamilyProjectionMode::CertifiedAppend,
            direct_append: true,
            preflight_bytes: TEST_RECORD.len() as u64,
            projection_bytes: TEST_RECORD.len() as u64,
            projected_records: 1,
        }
    );
}

#[test]
fn append_only_contract_reads_the_suffix_without_reauthenticating_old_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("trusted-append-only.jsonl");
    fs::write(&source_path, TEST_RECORD).unwrap();
    let adapter = DirectAppendTestAdapter::default();
    capture_parallel_test_generation(&adapter, &root, &index, 1);

    let mut rewritten_prefix = TEST_RECORD.to_vec();
    rewritten_prefix[1] ^= 1;
    assert_eq!(rewritten_prefix.len(), TEST_RECORD.len());
    let mut rewritten_and_appended = rewritten_prefix;
    rewritten_and_appended.extend_from_slice(TEST_RECORD);
    fs::write(&source_path, rewritten_and_appended).unwrap();

    let prefix_hash = track_jsonl_prefix_hash_bytes(source_path);
    let (appended, activity) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(prefix_hash.bytes(), 0);
    assert_eq!(activity.sources_started, 1);
    assert_eq!(appended.manifest().sources[0].counts().complete_records, 2);
    assert_eq!(
        *adapter.observations.lock().unwrap().last().unwrap(),
        DirectAppendPassObservation {
            mode: JsonlFamilyProjectionMode::CertifiedAppend,
            direct_append: true,
            preflight_bytes: TEST_RECORD.len() as u64,
            projection_bytes: TEST_RECORD.len() as u64,
            projected_records: 1,
        }
    );
}

#[test]
fn exhaustive_reconciliation_authenticates_and_replaces_a_rewritten_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("exhaustively-reconciled.jsonl");
    fs::write(&source_path, TEST_RECORD).unwrap();
    let adapter = DirectAppendTestAdapter::default();
    capture_parallel_test_generation(&adapter, &root, &index, 1);

    let replacement = b"{\"message\":\"after!\"}\n";
    assert_eq!(replacement.len(), TEST_RECORD.len());
    let mut rewritten_and_appended = replacement.to_vec();
    rewritten_and_appended.extend_from_slice(TEST_RECORD);
    fs::write(&source_path, rewritten_and_appended).unwrap();

    let (reconciled, activity) =
        capture_parallel_test_generation_exhaustive_with_terminal_revalidation(
            &adapter, &root, &index, 1,
        )
        .unwrap();
    assert_eq!(activity.sources_started, 1);
    assert_eq!(activity.sources_completed, 1);
    assert_eq!(
        reconciled.manifest().sources[0].counts().complete_records,
        2
    );
    let observation = *adapter.observations.lock().unwrap().last().unwrap();
    assert_eq!(observation.mode, JsonlFamilyProjectionMode::Replacement);
    assert!(!observation.direct_append);
}

#[test]
fn unchanged_terminal_proof_fails_closed_on_prepublication_source_races() {
    let append_adapter = ParallelTestAdapter;
    let replacement_adapter = ReplacementParallelTestAdapter;
    for (proof_kind, adapter) in [
        (
            "frozen-prefix",
            &append_adapter as &JsonlFamilyAdapterObject,
        ),
        (
            "exact-file",
            &replacement_adapter as &JsonlFamilyAdapterObject,
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
                matches!(error, SourceIoError::SourceChangedDuringCapture),
                "{proof_kind} {race} produced {error:?}"
            );
            assert_eq!(
                jsonl_family_scanner_activity(),
                JsonlFamilyScannerActivity::default(),
                "{proof_kind} {race} did not take unchanged admission"
            );
            assert_eq!(cold.manifest().sources.len(), 1);
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
        FamilyCheckpoint::decode_frontier_key::<CaptureError>(checkpoint)
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


mod additional;
