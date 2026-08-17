use super::*;

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
