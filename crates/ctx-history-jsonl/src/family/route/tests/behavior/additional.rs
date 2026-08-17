use super::*;

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
    let mut writer = match IndexCaptureLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("test lifecycle unexpectedly requires recovery")
        }
    };
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut applied_removals = Vec::new();
    let mut sink = SourceBackedGenerationSink::new(
        &mut writer,
        &mut owners,
        &mut complete_inventories,
        &mut applied_removals,
        0,
        test_route_identity(),
        None,
        SourceBackedRouteResources::production(4),
        &mut logical_source_failures,
        &mut record_rejections,
        None,
        None,
        None,
    );

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
        let mut writer =
            match IndexCaptureLifecycle::open(&temp.path().join(format!("index-{workers}")), ())
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
        let mut applied_removals = Vec::new();
        let mut sink = SourceBackedGenerationSink::new(
            &mut writer,
            &mut owners,
            &mut complete_inventories,
            &mut applied_removals,
            0,
            test_route_identity(),
            None,
            SourceBackedRouteResources::for_test(workers, 1, u64::MAX),
            &mut logical_source_failures,
            &mut record_rejections,
            None,
            None,
            None,
        );

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
        normalized_jsonl_error_kind(&CaptureError::SystemInvariant("broken worker")),
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
