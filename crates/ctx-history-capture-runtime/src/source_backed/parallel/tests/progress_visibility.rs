use super::*;

#[test]
fn batch_application_preserves_order_accounts_progress_and_releases_reservations() {
    let temp = tempdir();
    let source = test_source(34);
    let records = (1..=3)
        .map(|sequence| test_core_record(&source, sequence, 34))
        .collect::<Vec<_>>();
    let admission_delay = Duration::from_millis(20);
    let mut harness =
        SinkHarness::with_lifecycle(FakeLifecycle::with_add_prepared_delay(admission_delay));
    let preparer = harness.writer.core_preparation();
    let prepared_sizes = records
        .iter()
        .cloned()
        .map(|record| u64::try_from(preparer.prepare(record).unwrap().encoded_bytes).unwrap())
        .collect::<Vec<_>>();
    let total_prepared_bytes = prepared_sizes.iter().copied().sum::<u64>();

    let shared_history = ctx_history_capture_model::SharedAttemptHistoryProgress::default();
    let resources = SourceBackedRouteResources::for_test(1, total_prepared_bytes, u64::MAX)
        .with_attempt_history_progress(shared_history.clone());
    let progress_resources = resources.clone();
    let mut progress = Vec::new();
    let mut progress_observed_at = Vec::new();
    let mut live_bytes_after_acceptance = Vec::new();
    let mut report_progress = |delta: SourceBackedRecordProgressDelta| {
        assert_eq!(
            shared_history.snapshot(),
            ctx_history_capture_model::AttemptHistoryProgressSnapshot {
                processed_sessions: 1,
                processed_messages: 3,
                processed_tool_calls: 0,
                processed_bytes: 512,
            },
        );
        shared_history.advance_coordinator(&delta);
        progress.push(delta);
        progress_observed_at.push(Instant::now());
        live_bytes_after_acceptance
            .push(progress_resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput));
        Ok(())
    };
    let job_source = source.clone();
    let emitted_records = records.clone();
    let started = Instant::now();
    let results = harness
        .run_with_resources_and_record_progress::<_, (), _>(
            vec![ParallelLeafScanJob::new(source.clone(), ())],
            1,
            resources.clone(),
            &mut report_progress,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                let mut emissions = CoreRecordEmissionBatchBuilder::default();
                emitter.emit_core_records_with_completed_bytes(
                    &mut emissions,
                    emitted_records.clone(),
                    512,
                )?;
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&job_source, 34, 3, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(results, [()]);
    assert_eq!(
        progress,
        vec![SourceBackedRecordProgressDelta {
            accepted_records: 3,
            completed_bytes: 512,
            exact_total_bytes: None,
            exact_completed_bytes: None,
            session_ids: Vec::new(),
            messages: 0,
            tool_calls: 0,
        }]
    );
    assert_eq!(
        live_bytes_after_acceptance,
        [0],
        "coordinator progress follows full batch admission"
    );
    assert!(
        progress_observed_at[0].saturating_duration_since(started)
            >= admission_delay.saturating_mul(3),
        "coordinator progress must wait for the delayed writer batch"
    );
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
    assert_eq!(shared_history.parallel_byte_debt(), 0);

    let batch_commit = harness.commit();

    let reference_root = temp.path().join("reference-index");
    let mut reference = SinkHarness::open(&reference_root);
    let reference_source = source.clone();
    let reference_records = records.clone();
    reference
        .run(
            vec![ParallelLeafScanJob::new(source, ())],
            1,
            move |job, emitter| {
                emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                for record in reference_records.clone() {
                    emitter.emit_core_record(record)?;
                }
                emitter.complete(ParallelLeafScanComplete::replace(
                    test_certificate(&reference_source, 34, 3, false),
                    (),
                ))?;
                Ok(())
            },
        )
        .unwrap();
    let reference_commit = reference.commit();
    assert_eq!(
        batch_commit.generation_id, reference_commit.generation_id,
        "one batch must preserve the canonical order of the equivalent single-record emissions"
    );
}

#[test]
fn next_parallel_page_is_visible_while_first_add_prepared_is_gated() {
    let first_source = test_source(41);
    let second_source = test_source(42);
    let first = test_core_record(&first_source, 1, 41);
    let second_message = test_core_record(&second_source, 2, 42);
    let mut second_tool = test_core_record(&second_source, 3, 42);
    second_tool.event_type = "tool_call".to_owned();

    let shared_history = ctx_history_capture_model::SharedAttemptHistoryProgress::default();
    let resources = SourceBackedRouteResources::production(2)
        .with_attempt_history_progress(shared_history.clone());
    let observed_resources = resources.clone();
    let (gate, add_prepared_entered, release_add_prepared) = AddPreparedGate::channel();
    let accepted_records = Arc::new(AtomicU64::new(0));
    let callback_count = Arc::new(AtomicUsize::new(0));
    let callback_accepted_records = Arc::clone(&accepted_records);
    let observed_callback_count = Arc::clone(&callback_count);
    let callback_history = shared_history.clone();
    let mut report_progress = move |delta: SourceBackedRecordProgressDelta| {
        callback_history.advance_coordinator(&delta);
        callback_accepted_records.fetch_add(delta.accepted_records, Ordering::SeqCst);
        observed_callback_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let mut harness = SinkHarness::with_lifecycle(FakeLifecycle::with_add_prepared_gate(gate));
    let producers_started = Arc::new(Barrier::new(2));
    let worker_producers_started = Arc::clone(&producers_started);
    let worker_add_prepared_entered = Arc::clone(&add_prepared_entered);

    std::thread::scope(|scope| {
        let runner = scope.spawn(move || {
            harness
                .run_with_resources_and_record_progress::<_, (), _>(
                    vec![
                        ParallelLeafScanJob::new(first_source.clone(), 0_u8),
                        ParallelLeafScanJob::new(second_source.clone(), 1_u8),
                    ],
                    2,
                    resources,
                    &mut report_progress,
                    move |job, emitter| {
                        emitter.begin(ParallelLeafScanBegin::replace(job.source().clone()))?;
                        // Ensure both producers have completed protocol begin
                        // before the first page can block the coordinator.
                        worker_producers_started.wait();
                        let document_count = if *job.leaf() == 0 {
                            emitter.emit_core_records_with_completed_bytes(
                                &mut CoreRecordEmissionBatchBuilder::default(),
                                vec![first.clone()],
                                100,
                            )?;
                            1
                        } else {
                            while !worker_add_prepared_entered.load(Ordering::SeqCst) {
                                std::thread::yield_now();
                            }
                            // This producer publishes two records sharing one
                            // session while the first actual add_prepared is
                            // channel-gated on the coordinator thread.
                            emitter.emit_core_records_with_completed_bytes(
                                &mut CoreRecordEmissionBatchBuilder::default(),
                                vec![second_message.clone(), second_tool.clone()],
                                200,
                            )?;
                            2
                        };
                        emitter.complete(ParallelLeafScanComplete::replace(
                            test_certificate(
                                job.source(),
                                41_u8.saturating_add(*job.leaf()),
                                document_count,
                                false,
                            ),
                            (),
                        ))?;
                        Ok(())
                    },
                )
                .map(|results| (harness, results))
        });

        let entered_deadline = Instant::now() + Duration::from_secs(5);
        while !add_prepared_entered.load(Ordering::SeqCst) {
            if Instant::now() >= entered_deadline {
                let _ = release_add_prepared.send(false);
                panic!("coordinator did not enter the first add_prepared gate");
            }
            std::thread::yield_now();
        }

        let expected = ctx_history_capture_model::AttemptHistoryProgressSnapshot {
            processed_sessions: 2,
            processed_messages: 2,
            processed_tool_calls: 1,
            processed_bytes: 300,
        };
        let publish_deadline = Instant::now() + Duration::from_secs(5);
        while shared_history.snapshot() != expected {
            if Instant::now() >= publish_deadline {
                let observed = shared_history.snapshot();
                let _ = release_add_prepared.send(false);
                panic!("page N+1 facts were not visible during gated admission: {observed:?}");
            }
            std::thread::yield_now();
        }
        assert_eq!(accepted_records.load(Ordering::SeqCst), 0);
        assert_eq!(callback_count.load(Ordering::SeqCst), 0);
        assert_eq!(shared_history.parallel_byte_debt(), 300);

        release_add_prepared.send(true).unwrap();
        let (harness, results) = runner.join().unwrap().unwrap();
        assert_eq!(results, [(), ()]);
        assert_eq!(harness.writer.records.len(), 3);
    });

    assert_eq!(accepted_records.load(Ordering::SeqCst), 3);
    assert_eq!(callback_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        shared_history.snapshot(),
        ctx_history_capture_model::AttemptHistoryProgressSnapshot {
            processed_sessions: 2,
            processed_messages: 2,
            processed_tool_calls: 1,
            processed_bytes: 300,
        }
    );
    assert_eq!(shared_history.parallel_byte_debt(), 0);
    assert_eq!(
        observed_resources.live_bytes(SourceBackedRouteResourceKind::CoreOutput),
        0
    );
}
