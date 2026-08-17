use super::*;

#[test]
fn failed_running_exact_remains_in_manual_all_successor_work() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x31), route_identity(0x32)]);
    let first_route = routes.iter().next().unwrap().clone();
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_first_route = first_route.clone();
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            let first = executor_calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                executor_entered.wait();
                executor_release.wait();
            } else {
                assert!(execution.covered_route_ids.is_empty());
            }
            publish_selected_routes(
                &execution,
                &selected,
                first.then_some((&executor_first_route, "unavailable")),
            )
        },
    )));
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(3, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            assert!(!runner.run_next(&runner_root).unwrap().failed);
        });
        entered.wait();
        let _manual = manual_all_request(&coordinator, &data_root, &authority);
        release.wait();
    });
    assert!(!coordinator.run_next(&data_root).unwrap().failed);

    let observed = scans.lock().unwrap();
    assert_eq!(observed.get(&first_route), Some(&2));
    for route in routes.iter().filter(|route| *route != &first_route) {
        assert_eq!(observed.get(route), Some(&2));
    }
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn event_during_running_exact_invalidates_manual_all_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x41), route_identity(0x42)]);
    let first_route = routes.iter().next().unwrap().clone();
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_entered.wait();
                executor_release.wait();
            } else {
                assert!(execution.covered_route_ids.is_empty());
            }
            publish_selected_routes(&execution, &selected, None)
        },
    )));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes(routes.clone(), EventWatermark::new(4, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            assert!(!runner.run_next(&runner_root).unwrap().failed);
        });
        entered.wait();
        let _manual = manual_all_request(&coordinator, &data_root, &authority);
        coordinator.record_watch_routes(
            [(first_route.clone(), EventWatermark::new(4, 1))],
            observed_at_ms,
        );
        release.wait();
    });
    assert!(!coordinator.run_next(&data_root).unwrap().failed);

    let observed = scans.lock().unwrap();
    assert_eq!(observed.get(&first_route), Some(&2));
    for route in routes.iter().filter(|route| *route != &first_route) {
        assert_eq!(observed.get(route), Some(&2));
    }
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn ordinary_manual_all_still_scans_every_current_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x45), route_identity(0x46)]);
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(execution.scope, SourceBackedRefreshScope::All);
            assert!(execution.covered_route_ids.is_empty());
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            publish_selected_routes(&execution, &selected, None)
        },
    ));
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(5, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let manual = manual_all_request(&coordinator, &data_root, &authority);

    let run = coordinator
        .run_next(&data_root)
        .expect("ordinary manual all");
    assert!(!run.failed);
    assert_eq!(request_id(&run.job), request_id(&manual));
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn exact_route_event_during_execution_creates_one_successor_and_noop_ack_cleans_it() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x51);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(
                execution.scope,
                SourceBackedRefreshScope::exact([executor_route.clone()])
            );
            let call = executor_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                executor_entered.wait();
                executor_release.wait();
            }
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            publication.published_explicit_source_catalog =
                execution.explicit_source_catalog.cloned();
            publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
                executor_route.as_str().to_owned(),
                true,
            )];
            Ok(publication)
        });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(executor));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_data_root = data_root.clone();
        scope.spawn(move || {
            let run = runner
                .run_next(&runner_data_root)
                .expect("first exact route run");
            assert!(!run.failed);
            assert!(matches!(run.scope, SourceBackedRefreshScope::Exact(_)));
        });
        entered.wait();
        coordinator
            .record_watch_routes([(route.clone(), EventWatermark::new(1, 1))], observed_at_ms);
        release.wait();
    });

    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let successor = coordinator
        .run_next(&data_root)
        .expect("successor exact route run");
    assert!(!successor.failed);
    assert!(!successor.did_work, "unchanged exact route must be a no-op");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
}

fn route_failure_executor(
    route: SourceRouteIdentity,
    class: &'static str,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        let commit = ctx_history_index::GenerationWriter::open(
            execution.index_root,
            WriterOptions::default(),
        )?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?
        .commit(|_| true)?;
        let mut publication = empty_test_publication(commit.generation_id);
        publication.published_explicit_source_catalog = execution.explicit_source_catalog.cloned();
        let mut result = SourceBackedRefreshRouteResult::failed(
            route.as_str().to_owned(),
            class.to_owned(),
            true,
        );
        result.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "cd".repeat(32),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
            source_selector: "fixture source".to_owned(),
            detail: "fixture failure".to_owned(),
        }];
        publication.route_results = vec![result];
        Ok(publication)
    })
}

#[test]
fn exact_route_receipt_failures_back_off_or_block_until_a_new_event() {
    let route = route_identity(0x61);
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);

    let retry_temp = tempfile::tempdir().unwrap();
    let retry_root = retry_temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&retry_root).unwrap();
    let retry =
        CoreRefreshEngine::with_executor(route_failure_executor(route.clone(), "unavailable"));
    retry.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());
    assert!(!retry.run_next(&retry_root).unwrap().failed);
    let retry_after = retry
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("retryable route remains scheduled");
    assert!(retry_after <= 10_000 && retry_after > 0);
    assert!(!retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());

    let blocked_temp = tempfile::tempdir().unwrap();
    let blocked_root = blocked_temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&blocked_root).unwrap();
    let blocked =
        CoreRefreshEngine::with_executor(route_failure_executor(route.clone(), "incompatible"));
    blocked.reconcile_watch_routes([route.clone()], EventWatermark::new(2, 0), observed_at_ms);
    assert!(blocked
        .enqueue_next_dirty_route(&blocked_root, ledger_now_ms())
        .unwrap());
    assert!(!blocked.run_next(&blocked_root).unwrap().failed);
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route.clone(), EventWatermark::new(2, 0))], observed_at_ms);
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route, EventWatermark::new(2, 1))], observed_at_ms);
    assert!(blocked.has_scheduled_route_work());
}

#[test]
fn failed_exhaustive_exact_predecessor_cancels_attached_broad_successor_and_retains_route_retry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x64);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let failed_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(
                execution.scope,
                SourceBackedRefreshScope::exact([failed_route.clone()])
            );
            executor_entered.wait();
            executor_release.wait();
            Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: SourceBackedSourceFailures::from_failures([
                    SourceBackedFailedRoute::new(
                        failed_route.clone(),
                        "65".repeat(32),
                        CaptureProvider::Codex,
                        SourceBackedSourceFailureClass::Unavailable,
                        false,
                        "fixture source",
                        "temporarily unavailable",
                    ),
                ]),
            }
            .into())
        });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(executor));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes([route.clone()], EventWatermark::new(8, 0), observed_at_ms);
    coordinator
        .enqueue_with_catalog_metadata(
            None,
            SourceRefreshRuntimeMetadata::periodic(),
            None,
            SourceBackedRefreshScope::exact([route.clone()]),
            SourceRefreshLogicalDemand {
                admission: SourceRefreshAdmissionRequirement::AttachEquivalent,
                reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
                route_observations: BTreeMap::new(),
                request_id: None,
                request_fingerprint: None,
                admission_pending: false,
            },
        )
        .unwrap();
    let logical_request_id = Uuid::from_u128(0x64).to_string();
    let authority = test_catalog_authority(8, 0);

    let physical = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_data_root = data_root.clone();
        let handle = scope.spawn(move || {
            runner
                .run_next(&runner_data_root)
                .expect("failed exact predecessor")
        });
        entered.wait();
        let attached = coordinator
            .enqueue_fresh_catalog_demand_for_test(
                &data_root,
                None,
                logical_request_id.clone(),
                authority,
            )
            .expect("attached broad logical successor");
        assert_eq!(attached["logical_phase"], "attached");
        release.wait();
        handle.join().unwrap()
    });

    assert!(physical.failed);
    let logical = coordinator
        .status(&logical_request_id)
        .expect("terminal logical successor");
    assert_eq!(logical["request_state"], "failed");
    assert_eq!(logical["logical_phase"], "terminal");
    assert_eq!(
        logical["structured_outcome"]["physical_attempt_id"],
        physical.job["request_id"]
    );
    assert_eq!(
        logical["structured_outcome"]["retryable_routes"],
        json!([route.as_str()])
    );
    assert!(!coordinator.has_pending_request());
    assert_eq!(
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap()
            ["request_id"],
        logical_request_id
    );
    let mut global_retry = physical.job.clone();
    global_retry["retryable"] = json!(true);
    global_retry["retry_after_ms"] = json!(30_000);
    let authoritative = coordinator
        .persist_retry_status(&data_root, global_retry)
        .expect("route-local terminal authority");
    assert_eq!(authoritative["request_id"], logical_request_id);
    assert!(authoritative.get("retryable").is_none());
    assert!(authoritative.get("retry_after_ms").is_none());
    assert_eq!(
        coordinator.dirty_route_ids_for_test(),
        BTreeSet::from([route.clone()])
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&route));
    assert!(coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .is_some_and(|delay| delay > 0));
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
}

#[test]
fn successful_partial_publication_retains_mixed_route_retry_dispositions() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let retryable_route = route_identity(0x62);
    let blocked_route = route_identity(0x63);
    let routes = BTreeSet::from([retryable_route.clone(), blocked_route.clone()]);
    let executor_retryable_route = retryable_route.clone();
    let executor_blocked_route = blocked_route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            publication.route_results = [
                (&executor_retryable_route, "unavailable"),
                (&executor_blocked_route, "incompatible"),
            ]
            .into_iter()
            .map(|(route, class)| {
                let mut result =
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false);
                result.source_failure_total = 1;
                result.source_failures = vec![SourceBackedRefreshSourceFailure {
                    route_identity: route.as_str().to_owned(),
                    source_identity: "ef".repeat(32),
                    provider: "fixture".to_owned(),
                    class: class.to_owned(),
                    carried_forward: true,
                    source_selector: "fixture logical source".to_owned(),
                    detail: "fixture partial source failure".to_owned(),
                }];
                result
            })
            .collect();
            Ok(publication)
        });
    let coordinator = CoreRefreshEngine::with_executor(executor);
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes(routes.clone(), EventWatermark::new(4, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    let run = coordinator
        .run_next(&data_root)
        .expect("partial publication");

    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(
        run.job["structured_outcome"]["code"],
        "completed_with_source_failures"
    );
    assert_eq!(run.job["structured_outcome"]["retryable"], true);
    assert_eq!(
        run.job["structured_outcome"]["retryable_routes"],
        json!([retryable_route.as_str()])
    );
    assert_eq!(
        run.job["structured_outcome"]["blocked_routes"],
        json!([blocked_route.as_str()])
    );
    assert_eq!(
        run.job["structured_outcome"]["retained_generation"],
        run.job["published_generation"]
    );
    assert_eq!(
        run.job["structured_outcome"]["published_generation"],
        run.job["published_generation"]
    );
    assert_eq!(coordinator.dirty_route_ids_for_test(), routes);
    assert!(!coordinator.route_is_permanently_blocked_for_test(&retryable_route));
    assert!(coordinator.route_is_permanently_blocked_for_test(&blocked_route));
    assert!(coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .is_some());
    coordinator.record_watch_routes(
        [(blocked_route.clone(), EventWatermark::new(4, 1))],
        observed_at_ms,
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&blocked_route));
}

#[test]
fn systemic_exact_publication_failure_leaves_the_route_dirty_with_backoff() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x71);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(|_: SourceBackedRefreshExecution<'_>| Err(anyhow!("systemic fixture failure")));
    let coordinator = CoreRefreshEngine::with_executor(executor);
    coordinator.reconcile_watch_routes(
        [route],
        EventWatermark::new(3, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    assert!(coordinator.run_next(&data_root).unwrap().failed);
    let retry_after = coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("systemic failure remains dirty");
    assert!(retry_after <= 10_000 && retry_after > 0);
}

#[path = "tests/receipt.rs"]
mod receipt_tests;

#[test]
fn differing_catalog_authority_queues_one_successor_behind_a_running_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let first_authority = test_catalog_authority(1, 0x11);
    let second_authority = test_catalog_authority(2, 0x22);
    let request = |authority: &ExplicitSourceCatalogAuthority| {
        coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let first = request(&first_authority);
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let (second, second_replay) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_request_id = first_request_id.clone();
        let runner_authority = first_authority.clone();
        scope.spawn(move || {
            let first_run = runner
                .run_next_with(
                    |request_id, _| {
                        assert_eq!(request_id, runner_request_id);
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("catalog-generation-1");
                        publication.published_explicit_source_catalog = Some(runner_authority);
                        Ok(publication)
                    },
                    || Ok(Some("catalog-generation-1".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running first catalog refresh");
            assert!(!first_run.failed);
        });
        gate.wait_until_started();

        let second = request(&second_authority);
        let second_replay = request(&second_authority);
        gate.release();
        (second, second_replay)
    });

    let second_request_id = request_id(&second);
    assert_ne!(first_request_id, second_request_id);
    assert_eq!(request_id(&second_replay), second_request_id);
    assert_eq!(second_replay["coalesced_requests"], 1);
    assert_eq!(second["request_state"], "queued");
    assert_eq!(
        coordinator.status(&first_request_id).unwrap()["request_state"],
        "published"
    );
    let queued_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(queued_second["request_state"], "queued");
    assert_eq!(queued_second["previous_generation"], "catalog-generation-1");

    let second_run = coordinator
        .run_next_with(
            |request_id, _| {
                assert_eq!(request_id, second_request_id);
                let mut publication = test_publication("catalog-generation-2");
                publication.published_explicit_source_catalog = Some(second_authority.clone());
                Ok(publication)
            },
            || Ok(Some("catalog-generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!second_run.failed);
    assert!(!coordinator.has_pending_request());
    let published_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(published_second["request_state"], "published");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(
            &published_second["receipt"]["published_explicit_source_catalog"]
        )
        .unwrap(),
        second_authority
    );
}

#[test]
fn active_and_pending_refreshes_are_bounded_with_a_typed_busy_response() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = |revision: u64| {
        let digest_byte = u8::try_from(revision).unwrap();
        let authority = test_catalog_authority(revision, digest_byte);
        coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let accepted = (1..=SOURCE_REFRESH_ACTIVE_PENDING_LIMIT)
        .map(|revision| request(u64::try_from(revision).unwrap()))
        .collect::<Vec<_>>();
    assert!(accepted.iter().all(|response| response["ok"] == true));
    assert_eq!(
        accepted
            .iter()
            .map(request_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );

    let busy = request(99);
    assert_eq!(busy["ok"], false);
    assert_eq!(busy["status"], "busy");
    assert_eq!(busy["error_code"], "source_refresh_queue_full");
    assert_eq!(busy["reason"], "queue_full");
    assert_eq!(busy["retryable"], true);
    assert_eq!(
        busy["active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert_eq!(
        busy["max_active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert!(busy.get("request_id").is_none());
}

#[test]
fn terminal_history_is_trimmed_independently_from_inflight_capacity() {
    let coordinator = CoreRefreshEngine::new();
    let total = SOURCE_REFRESH_ATTEMPT_HISTORY + 3;
    let mut request_ids = Vec::with_capacity(total);

    for generation in 0..total {
        let previous = format!("generation-{generation}");
        let published = format!("generation-{}", generation.saturating_add(1));
        let request = coordinator.enqueue(Some(previous));
        request_ids.push(request_id(&request));
        let run = coordinator
            .run_next_with(
                |_, _| Ok(test_publication(published.clone())),
                || Ok(Some(published.clone())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("queued refresh");
        assert!(!run.failed);
    }

    assert!(request_ids[..3]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_none()));
    assert!(request_ids[3..]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_some()));

    let next = coordinator.enqueue(Some(format!("generation-{total}")));
    assert_eq!(next["request_state"], "queued");
    assert!(coordinator.has_pending_request());
}

#[test]
fn production_run_persists_discovering_before_executor_entry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_from_executor = Arc::clone(&observed);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let job =
                read_daemon_job_status(&daemon_source_backed_refresh_job_path(execution.data_root))
                    .expect("running source refresh status");
            assert_eq!(job["request_state"], "running");
            assert_eq!(job["progress"]["phase"], "discovering");
            assert_eq!(job["progress"]["total_sources_known"], false);
            assert!(job["progress"]["current_source"].is_null());
            assert!(job["progress"]["completed_records"].is_null());
            assert!(job["progress"]["completed_bytes"].is_null());
            assert!(job["progress"]["current_source_progress"].is_null());
            observed_from_executor.store(true, Ordering::SeqCst);
            Err(anyhow!("stop after observing persisted discovery phase"))
        },
    ));
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let _request = manual_all_request(&coordinator, &data_root, &authority);

    let run = coordinator.run_next(&data_root).expect("queued refresh");
    assert!(run.failed);
    assert!(observed.load(Ordering::SeqCst));
}

#[test]
fn default_executor_uses_capture_owned_execution() {
    let coordinator = CoreRefreshEngine::new();
    assert_eq!(
        coordinator.executor.implementation_name(),
        std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
    );
}
#[path = "tests/unsupported_refresh.rs"]
mod unsupported_refresh;

#[path = "tests/codex_union.rs"]
mod codex_union_tests;

#[path = "tests/request_coalescing.rs"]
mod request_coalescing_tests;

#[path = "tests/publication_lifecycle.rs"]
mod publication_lifecycle_tests;

#[path = "tests/durable_receipt.rs"]
mod durable_receipt_tests;
