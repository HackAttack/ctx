use super::*;

#[test]
fn duplicate_pre_turn_provider_identity_is_unknown_on_cold_and_append_restart() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let parent = "019fb000-0000-7000-8000-000000000025";
    let child = "019fb000-0000-7000-8000-000000000026";
    let duplicated = [
        exec_call("duplicate-pre-turn-call"),
        exec_result("duplicate-pre-turn-call", "duplicate-pre-turn-first"),
        exec_call("duplicate-pre-turn-call"),
        exec_result("duplicate-pre-turn-call", "duplicate-pre-turn-second"),
    ];
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        duplicated.clone(),
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Forked,
        Some(parent),
        duplicated,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&index_root).unwrap();
    let duplicate_records = records_for(&cold, child)
        .into_iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("duplicate-pre-turn"))
        })
        .collect::<Vec<_>>();
    assert_eq!(duplicate_records.len(), 2);
    assert!(duplicate_records
        .iter()
        .all(|record| record.event_origin == EventOrigin::Unknown));
    drop(cold);

    append_event(
        &session_path(&sessions, child),
        message("duplicate-provider-append-restart"),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_eq!(sources.get(child).unwrap().counters.appended_sources, 1);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    assert!(records_for(&appended, child)
        .iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("duplicate-pre-turn"))
        })
        .all(|record| record.event_origin == EventOrigin::Unknown));
    let appended_certificate = serde_json::to_vec(&certificate_for(&appended, child)).unwrap();
    drop(appended);

    let cold_index_root = temp.path().join("cold-index");
    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let cold_restart = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&cold_restart, child)).unwrap(),
        appended_certificate
    );
}

#[test]
fn unmatched_or_ambiguous_call_ids_suppress_exact_outcomes() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let command = "git commit -m exact && git rev-parse HEAD";
    let root = "019fb000-0000-7000-8000-000000000030";
    write_session(
        &sessions,
        root,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call_in("ambiguous-call", command, &repository),
            exec_call_in("ambiguous-call", command, &repository),
            successful_result(
                "ambiguous-call",
                format!("ambiguous-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("matched-call", command, &repository),
            successful_result(
                "mismatched-result-call",
                format!("mismatched-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    let root_records = records_for(&verified, root);
    let ambiguous = root_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ambiguous-result-marker"))
        })
        .expect("ambiguous result record");
    assert_eq!(ambiguous.event_origin, EventOrigin::Unknown);
    assert!(ambiguous.repository_vcs_observations.is_empty());
    let mismatched = root_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("mismatched-result-marker"))
        })
        .expect("mismatched result record");
    assert_eq!(mismatched.event_origin, EventOrigin::Unknown);
    assert!(mismatched.repository_vcs_observations.is_empty());
}

#[test]
fn cold_direct_repository_results_require_exact_candidate_multiplicity() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000033";
    let mut events = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("direct-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    events.extend([
        successful_result(
            "direct-pre-result",
            format!("direct-pre-result-before\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-pre-result", command, &repository),
        successful_result(
            "direct-pre-result",
            format!("direct-pre-result-after\n[main abc1234] exact\n{oid}\n"),
        ),
        unrelated_tool_call("direct-pre-call"),
        exec_call_in("direct-pre-call", command, &repository),
        successful_result(
            "direct-pre-call",
            format!("direct-pre-call-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-duplicate", command, &repository),
        successful_result(
            "direct-duplicate",
            format!("direct-duplicate-same\n[main abc1234] exact\n{oid}\n"),
        ),
        successful_result(
            "direct-duplicate",
            format!("direct-duplicate-same\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-conflict", command, &repository),
        successful_result(
            "direct-conflict",
            format!("direct-conflict-first\n[main abc1234] exact\n{oid}\n"),
        ),
        successful_result(
            "direct-conflict",
            "direct-conflict-second\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n"
                .to_owned(),
        ),
        exec_call_in("direct-serial", command, &repository),
        successful_result(
            "direct-serial",
            format!("direct-serial-first\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-serial", command, &repository),
        successful_result(
            "direct-serial",
            format!("direct-serial-second\n[main abc1234] exact\n{oid}\n"),
        ),
    ]);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        events,
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&verified, native_session_id),
        &[
            "direct-pre-result-before",
            "direct-pre-result-after",
            "direct-pre-call-after",
            "direct-duplicate-same",
            "direct-conflict-first",
            "direct-conflict-second",
            "direct-serial-first",
            "direct-serial-second",
        ],
    );
}

#[test]
fn cold_continued_repository_results_require_exact_candidate_multiplicity() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000034";
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            completed_wait_result(
                "continued-pre-result-wait",
                format!("continued-pre-result-before\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-pre-result-origin", command, &repository),
            running_result("continued-pre-result-origin", "continued-pre-result-cell"),
            wait_call("continued-pre-result-wait", "continued-pre-result-cell"),
            completed_wait_result(
                "continued-pre-result-wait",
                format!("continued-pre-result-after\n[main abc1234] exact\n{oid}\n"),
            ),
            unrelated_tool_call("continued-pre-call-wait"),
            exec_call_in("continued-pre-call-origin", command, &repository),
            running_result("continued-pre-call-origin", "continued-pre-call-cell"),
            wait_call("continued-pre-call-wait", "continued-pre-call-cell"),
            completed_wait_result(
                "continued-pre-call-wait",
                format!("continued-pre-call-after\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-duplicate-origin", command, &repository),
            running_result("continued-duplicate-origin", "continued-duplicate-cell"),
            wait_call("continued-duplicate-wait", "continued-duplicate-cell"),
            completed_wait_result(
                "continued-duplicate-wait",
                format!("continued-duplicate-same\n[main abc1234] exact\n{oid}\n"),
            ),
            completed_wait_result(
                "continued-duplicate-wait",
                format!("continued-duplicate-same\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-conflict-origin", command, &repository),
            running_result("continued-conflict-origin", "continued-conflict-cell"),
            wait_call("continued-conflict-wait", "continued-conflict-cell"),
            completed_wait_result(
                "continued-conflict-wait",
                format!("continued-conflict-first\n[main abc1234] exact\n{oid}\n"),
            ),
            completed_wait_result(
                "continued-conflict-wait",
                "continued-conflict-second\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n",
            ),
            exec_call_in("continued-serial-origin", command, &repository),
            running_result("continued-serial-origin", "continued-serial-cell"),
            wait_call("continued-serial-wait", "continued-serial-cell"),
            completed_wait_result(
                "continued-serial-wait",
                format!("continued-serial-first\n[main abc1234] exact\n{oid}\n"),
            ),
            wait_call("continued-serial-wait", "continued-serial-cell"),
            completed_wait_result(
                "continued-serial-wait",
                format!("continued-serial-second\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&verified, native_session_id),
        &[
            "continued-pre-result-before",
            "continued-pre-result-after",
            "continued-pre-call-after",
            "continued-duplicate-same",
            "continued-conflict-first",
            "continued-conflict-second",
            "continued-serial-first",
            "continued-serial-second",
        ],
    );
}

#[test]
fn append_restart_counts_candidate_id_occurrences_before_first_admission() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let cold_index_root = temp.path().join("cold-index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = repository_head(&repository);
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000036";
    let path = session_path(&sessions, native_session_id);
    let mut prefix = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("late-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    prefix.extend([
        successful_result(
            "late-direct-result",
            format!("late-direct-before\n[main abc1234] exact\n{oid}\n"),
        ),
        unrelated_tool_call("late-direct-call"),
        completed_wait_result(
            "late-continued-wait",
            format!("late-continued-before\n[main abc1234] exact\n{oid}\n"),
        ),
    ]);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        prefix,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    for event in [
        exec_call_in("late-direct-result", command, &repository),
        successful_result(
            "late-direct-result",
            format!("late-direct-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("late-direct-call", command, &repository),
        successful_result(
            "late-direct-call",
            format!("late-call-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("late-continued-origin", command, &repository),
        running_result("late-continued-origin", "late-continued-cell"),
        wait_call("late-continued-wait", "late-continued-cell"),
        completed_wait_result(
            "late-continued-wait",
            format!("late-continued-after\n[main abc1234] exact\n{oid}\n"),
        ),
    ] {
        append_event(&path, event);
    }

    let observed = capture_causal_stage();
    refresh_source_backed_generation_incremental_for_test(&index_root, &registry, writer_options())
        .unwrap();
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 0);
    assert_eq!(counters.replaced_sources, 1);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&appended, native_session_id),
        &[
            "late-direct-before",
            "late-direct-after",
            "late-call-after",
            "late-continued-before",
            "late-continued-after",
        ],
    );
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    drop(appended);

    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let restarted = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&restarted, native_session_id),
        &[
            "late-direct-after",
            "late-call-after",
            "late-continued-after",
        ],
    );
    assert_eq!(
        serde_json::to_vec(&certificate_for(&restarted, native_session_id)).unwrap(),
        appended_certificate
    );
}

#[test]
fn append_restart_retracts_direct_and_continued_candidate_reuse_after_large_prefix() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let cold_index_root = temp.path().join("cold-index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = repository_head(&repository);
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000035";
    let path = session_path(&sessions, native_session_id);
    let mut events = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("append-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    for kind in ["duplicate", "conflict", "serial"] {
        let call_id = format!("append-direct-{kind}");
        events.push(exec_call_in(&call_id, command, &repository));
        events.push(successful_result(
            &call_id,
            format!("append-direct-{kind}-initial\n[main abc1234] exact\n{oid}\n"),
        ));
    }
    for kind in ["duplicate", "conflict", "serial"] {
        let origin = format!("append-continued-{kind}-origin");
        let cell = format!("append-continued-{kind}-cell");
        let wait = format!("append-continued-{kind}-wait");
        events.push(exec_call_in(&origin, command, &repository));
        events.push(running_result(&origin, &cell));
        events.push(wait_call(&wait, &cell));
        events.push(completed_wait_result(
            &wait,
            format!("append-continued-{kind}-initial\n[main abc1234] exact\n{oid}\n"),
        ));
    }
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        events,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_exact_commit_causality(
        &records_for(&initial, native_session_id),
        &[
            "append-direct-duplicate-initial",
            "append-direct-conflict-initial",
            "append-direct-serial-initial",
            "append-continued-duplicate-initial",
            "append-continued-conflict-initial",
            "append-continued-serial-initial",
        ],
    );
    drop(initial);

    append_event(
        &path,
        successful_result(
            "append-direct-duplicate",
            format!("append-direct-duplicate-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        successful_result(
            "append-direct-conflict",
            "append-direct-conflict-again\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n"
                .to_owned(),
        ),
    );
    append_event(
        &path,
        exec_call_in("append-direct-serial", command, &repository),
    );
    append_event(
        &path,
        successful_result(
            "append-direct-serial",
            format!("append-direct-serial-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-duplicate-wait",
            format!("append-continued-duplicate-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-conflict-wait",
            "append-continued-conflict-again\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n",
        ),
    );
    append_event(
        &path,
        wait_call(
            "append-continued-serial-wait",
            "append-continued-serial-cell",
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-serial-wait",
            format!("append-continued-serial-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );

    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&observed)
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_records = records_for(&appended, native_session_id);
    assert_no_repository_causality(
        &appended_records,
        &[
            "append-direct-duplicate-initial",
            "append-direct-duplicate-again",
            "append-direct-conflict-initial",
            "append-direct-conflict-again",
            "append-direct-serial-initial",
            "append-direct-serial-again",
            "append-continued-duplicate-initial",
            "append-continued-duplicate-again",
            "append-continued-conflict-initial",
            "append-continued-conflict-again",
            "append-continued-serial-initial",
            "append-continued-serial-again",
        ],
    );
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    drop(appended);

    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let restarted = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&restarted, native_session_id),
        &[
            "append-direct-duplicate-initial",
            "append-direct-conflict-initial",
            "append-direct-serial-initial",
            "append-continued-duplicate-initial",
            "append-continued-conflict-initial",
            "append-continued-serial-initial",
        ],
    );
    assert_eq!(
        serde_json::to_vec(&certificate_for(&restarted, native_session_id)).unwrap(),
        appended_certificate
    );
}

#[test]
fn fallback_identity_is_rewrite_stable_and_duplicate_occurrences_remain_distinct() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-000000000027";
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("fallback-stable-first"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-last"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    let initial_records = records_for(&initial, native_session_id);
    let initial_duplicates = initial_records
        .iter()
        .filter(|record| {
            record.content.normalized_body.as_deref() == Some("fallback-stable-duplicate")
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(initial_duplicates.len(), 2);
    assert_ne!(initial_duplicates[0], initial_duplicates[1]);
    let initial_stable = initial_records
        .iter()
        .filter(|record| {
            matches!(
                record.content.normalized_body.as_deref(),
                Some("fallback-stable-first" | "fallback-stable-last")
            )
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    drop(initial);

    replace_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("fallback-inserted-before"),
            message("fallback-stable-first"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-last"),
        ],
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_eq!(
        sources
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    let rewritten_records = records_for(&rewritten, native_session_id);
    let rewritten_duplicates = rewritten_records
        .iter()
        .filter(|record| {
            record.content.normalized_body.as_deref() == Some("fallback-stable-duplicate")
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    let rewritten_stable = rewritten_records
        .iter()
        .filter(|record| {
            matches!(
                record.content.normalized_body.as_deref(),
                Some("fallback-stable-first" | "fallback-stable-last")
            )
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(rewritten_duplicates, initial_duplicates);
    assert_eq!(rewritten_stable, initial_stable);
}

#[test]
fn direct_source_rewrite_delete_and_reappearance_replace_only_that_source() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-000000000028";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("oldsourceuniquetoken"),
            message("staledocumentuniquetoken"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("newsourceuniquetoken")],
    );
    let replacement = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&replacement)
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    assert!(rewritten
        .search_event_candidates("staledocumentuniquetoken", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        rewritten
            .search_event_candidates("newsourceuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    drop(rewritten);

    fs::remove_file(&path).unwrap();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let deleted = VerifiedIndex::open(&index_root).unwrap();
    assert!(deleted.manifest().sources.is_empty());
    assert!(deleted
        .search_event_candidates("newsourceuniquetoken", 8)
        .unwrap()
        .is_empty());
    drop(deleted);

    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("reappearedsourceuniquetoken")],
    );
    let reappeared = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&reappeared)
            .get(native_session_id)
            .unwrap()
            .counters
            .cold_sources,
        1
    );
}

#[test]
fn semantic_preflight_rewrite_cannot_publish_stale_mcp_or_repository_authority() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let native_session_id = "019fb000-0000-7000-8000-000000000044";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("preflightbindinglastgoodtoken")],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let last_good = VerifiedIndex::open(&index_root).unwrap();
    let last_good_generation = last_good.generation_id().to_owned();
    let last_good_snapshot = source_snapshot(
        &last_good,
        native_session_id,
        "preflightbindinglastgoodtoken",
    );
    drop(last_good);

    let fixture = |second_repository_call_id: &str, second_mcp_call_id: &str| {
        let mut metadata = session_meta(native_session_id, SessionRelationshipKind::Root, None);
        metadata["payload"]
            .as_object_mut()
            .unwrap()
            .remove("cli_version");
        jsonl_bytes([
            metadata,
            turn_context(),
            exec_call_in(
                "semantic-repo-call-a",
                "git commit -m semantic-race && git rev-parse HEAD",
                &repository,
            ),
            successful_result(
                "semantic-repo-call-a",
                format!("stalerepoauthorityfirsttoken\n[main abc1234] semantic-race\n{oid}\n"),
            ),
            exec_call_in(
                second_repository_call_id,
                "git commit -m semantic-race && git rev-parse HEAD",
                &repository,
            ),
            successful_result(
                second_repository_call_id,
                format!("stalerepoauthoritysecondtoken\n[main abc1234] semantic-race\n{oid}\n"),
            ),
            mcp_terminal(
                "semantic-mcp-call-a",
                "semantic-server-a",
                "stalemcpauthorityfirsttoken",
            ),
            mcp_terminal(
                second_mcp_call_id,
                "semantic-server-b",
                "stalemcpauthoritysecondtoken",
            ),
        ])
    };
    let admitted_a = fixture("semantic-repo-call-b", "semantic-mcp-call-b");
    let rewritten_b = fixture("semantic-repo-call-a", "semantic-mcp-call-a");
    assert_ne!(admitted_a, rewritten_b);
    assert_eq!(admitted_a.len(), rewritten_b.len());
    fs::write(&path, &admitted_a).unwrap();
    let hook_path = path.clone();
    set_after_jsonl_semantic_preflight_hook(path.clone(), move || {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(hook_path)
            .unwrap();
        file.write_all(&rewritten_b).unwrap();
        file.sync_all().unwrap();
    });

    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert!(failed.failed_routes[0].carried_forward);
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), last_good_generation);
    assert_eq!(
        source_snapshot(
            &retained,
            native_session_id,
            "preflightbindinglastgoodtoken"
        ),
        last_good_snapshot
    );
    for marker in [
        "stalerepoauthorityfirsttoken",
        "stalerepoauthoritysecondtoken",
        "stalemcpauthorityfirsttoken",
        "stalemcpauthoritysecondtoken",
    ] {
        assert!(
            retained
                .search_event_candidates(marker, 8)
                .unwrap()
                .is_empty(),
            "inter-pass rewrite published stale-authority record {marker}"
        );
    }
    drop(retained);

    let fresh = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(fresh.failed_routes.is_empty());
    assert!(fresh.logical_source_failures.is_empty());
    let rebound = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&rebound, native_session_id);
    assert_no_repository_causality(
        &records,
        &[
            "stalerepoauthorityfirsttoken",
            "stalerepoauthoritysecondtoken",
        ],
    );
    let mcp_records = records
        .iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("stalemcpauthority"))
        })
        .collect::<Vec<_>>();
    assert_eq!(mcp_records.len(), 2);
    assert!(mcp_records
        .iter()
        .all(|record| record.mcp_tool_call.is_none()));
}

#[test]
fn continuation_restart_preserves_exact_result_linkage_and_origin_proof() {
    assert_continuation_restart_exact_commit(SessionRelationshipKind::Root, None, false);
}

#[test]
fn missing_parent_local_continuation_restart_retains_exact_commit_origin() {
    assert_continuation_restart_exact_commit(
        SessionRelationshipKind::Forked,
        Some("019fb000-0000-7000-8000-000000000028"),
        true,
    );
}

#[test]
fn parser_revision_migration_rescans_once_without_catalog_body_hydration() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let parent = "019fb000-0000-7000-8000-000000000031";
    let child = "019fb000-0000-7000-8000-000000000032";
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("migration-parent-marker")],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("migration-child-marker")],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let current = VerifiedIndex::open(&index_root).unwrap();
    let old_routes = current.manifest().source_routes().to_vec();
    let old_sources = current
        .manifest()
        .sources
        .iter()
        .map(|certificate| {
            let native_session_id = match certificate.observation().source().anchor() {
                SourceAnchor::ProviderNative {
                    key: TypedKey::Utf8(value),
                    ..
                } => value,
                anchor => panic!("unexpected Codex source anchor {anchor:?}"),
            };
            let old_certificate = CertifiedSource::certify_with_frontier(
                certificate.observation().clone(),
                certificate.observation().clone(),
                "codex-nativepath-core-record-v27-bounded-exact-origin",
                *certificate.content_digest(),
                certificate.counts(),
                certificate.frontier().cloned(),
            )
            .unwrap();
            (old_certificate, records_for(&current, native_session_id))
        })
        .collect::<Vec<_>>();
    drop(current);
    let mut downgrade = GenerationWriter::open(&index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    downgrade
        .set_source_route_plan(
            old_routes
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        )
        .unwrap();
    for route in &old_routes {
        downgrade
            .begin_source_route_stage(route.route_identity().clone())
            .unwrap();
        for source in route.sources() {
            let (certificate, records) = old_sources
                .iter()
                .find(|(certificate, _)| {
                    certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(source)
                })
                .expect("route source has a retired-revision candidate");
            downgrade
                .begin_source(certificate.observation().source().clone())
                .unwrap();
            for record in records {
                downgrade.add_core_record(record.clone()).unwrap();
            }
            downgrade.certify_source(certificate.clone()).unwrap();
        }
        downgrade
            .finish_source_route_stage(route.route_identity())
            .unwrap();
    }
    downgrade.set_present_source_routes(old_routes).unwrap();
    downgrade
        .commit(|target| match target {
            RevalidationTarget::Source(actual) => {
                old_sources.iter().any(|(expected, _)| expected == actual)
            }
            RevalidationTarget::Deletion(_) => false,
        })
        .unwrap();

    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    for native_session_id in [parent, child] {
        let counters = sources.get(native_session_id).unwrap().counters;
        assert_eq!(counters.catalog_source_metadata_opens, 0);
        assert_eq!(counters.catalog_source_metadata_read_upper_bound_bytes, 0);
        assert_eq!(counters.catalog_session_meta_parses, 0);
        assert_eq!(counters.scanner_source_opens, 1);
        assert_eq!(counters.scanner_sources_started, 1);
        assert_eq!(counters.scanner_sources_completed, 1);
        assert_eq!(counters.replaced_sources, 1);
        assert_eq!(counters.writer_mutated_sources, 1);
    }
    let migrated = VerifiedIndex::open(&index_root).unwrap();
    for certificate in &migrated.manifest().sources {
        assert_eq!(certificate.parser_revision(), CURRENT_PARSER_REVISION);
        let frontier = certificate.frontier().unwrap();
        assert_eq!(frontier.checkpoint_kind(), CURRENT_FRONTIER_KIND);
        let TypedKey::Utf8(json) = frontier.checkpoint() else {
            panic!("Codex family checkpoint must be compact UTF-8");
        };
        let wire = serde_json::from_str::<serde_json::Value>(json).unwrap();
        assert_eq!(wire["version"], 5);
        assert_current_provider_checkpoint(&wire["provider_checkpoint"]);
        assert!(wire.get("certified_lineage_facts").is_none());
        assert!(wire.get("dependency_digest").is_none());
    }
}

#[test]
fn current_parser_legacy_codex_frontier_migrates_by_full_replacement() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-000000000039";
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("legacy-frontier-migration-marker")],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let current = VerifiedIndex::open(&index_root).unwrap();
    let old_routes = current.manifest().source_routes().to_vec();
    let current_certificate = certificate_for(&current, native_session_id);
    assert_eq!(
        current_certificate.parser_revision(),
        CURRENT_PARSER_REVISION
    );
    let current_frontier = current_certificate.frontier().unwrap();
    let legacy_frontier = SourceFrontier::new(
        LEGACY_CODEX_FRONTIER_KIND,
        current_frontier.checkpoint().clone(),
        current_frontier.certified_prefix_bytes(),
        *current_frontier.certified_prefix_digest(),
    )
    .unwrap();
    let legacy_certificate = CertifiedSource::certify_with_frontier(
        current_certificate.observation().clone(),
        current_certificate.observation().clone(),
        CURRENT_PARSER_REVISION,
        *current_certificate.content_digest(),
        current_certificate.counts(),
        Some(legacy_frontier),
    )
    .unwrap();
    assert_eq!(
        legacy_certificate.frontier().unwrap().checkpoint_kind(),
        LEGACY_CODEX_FRONTIER_KIND
    );
    let records = records_for(&current, native_session_id);
    drop(current);

    let mut install_legacy = GenerationWriter::open(&index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    install_legacy
        .set_source_route_plan(
            old_routes
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        )
        .unwrap();
    for route in &old_routes {
        install_legacy
            .begin_source_route_stage(route.route_identity().clone())
            .unwrap();
        install_legacy
            .begin_source(legacy_certificate.observation().source().clone())
            .unwrap();
        for record in &records {
            install_legacy.add_core_record(record.clone()).unwrap();
        }
        install_legacy
            .certify_source(legacy_certificate.clone())
            .unwrap();
        install_legacy
            .finish_source_route_stage(route.route_identity())
            .unwrap();
    }
    install_legacy
        .set_present_source_routes(old_routes)
        .unwrap();
    install_legacy
        .commit(|target| match target {
            RevalidationTarget::Source(actual) => actual == &legacy_certificate,
            RevalidationTarget::Deletion(_) => false,
        })
        .unwrap();

    let observed = capture_causal_stage();
    let migrated_receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(
        migrated_receipt.failed_routes.is_empty(),
        "legacy-frontier migration failed routes: {:?}",
        migrated_receipt.failed_routes
    );
    assert!(
        migrated_receipt.logical_source_failures.is_empty(),
        "legacy-frontier migration source failures: {:?}",
        migrated_receipt.logical_source_failures
    );
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.replaced_sources, 1);
    assert_eq!(counters.appended_sources, 0);
    assert_eq!(counters.writer_mutated_sources, 1);

    let migrated = VerifiedIndex::open(&index_root).unwrap();
    let migrated_certificate = certificate_for(&migrated, native_session_id);
    assert_eq!(
        migrated_certificate.parser_revision(),
        CURRENT_PARSER_REVISION
    );
    assert_eq!(
        migrated_certificate.frontier().unwrap().checkpoint_kind(),
        CURRENT_FRONTIER_KIND
    );
    assert_eq!(
        records_for(&migrated, native_session_id)
            .iter()
            .filter(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("legacy-frontier-migration-marker"))
            })
            .count(),
        1
    );
}

#[test]
fn cold_continuous_appends_during_frozen_prefix_admission_catch_up_once() {
    const INVENTORY_APPEND_MARKER: &str = "coldinventoryappendtoken306a";
    const TERMINAL_APPEND_MARKER: &str = "coldterminalappendtoken306b";
    const PRECOMMIT_APPEND_MARKER: &str = "coldprecommitappendtoken306c";

    let (_temp, sessions, index_root) = codex_test_workspace();
    let parent = "019fb000-0000-7000-8000-000000000041";
    let child = "019fb000-0000-7000-8000-000000000042";
    let parent_path = session_path(&sessions, parent);
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("coldprefixuniquetoken")],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("coldchildstableuniquetoken")],
    );
    let registry = register_tree(&[&sessions]);

    let metadata_inventory = Arc::new(Barrier::new(2));
    let terminal_observation = Arc::new(Barrier::new(2));
    let precommit_physical_revalidation = Arc::new(Barrier::new(2));
    let writer_path = parent_path.clone();
    let writer_metadata_inventory = Arc::clone(&metadata_inventory);
    let writer_terminal_observation = Arc::clone(&terminal_observation);
    let writer_precommit_physical_revalidation = Arc::clone(&precommit_physical_revalidation);
    let writer = std::thread::spawn(move || {
        writer_metadata_inventory.wait();
        append_event(&writer_path, message(INVENTORY_APPEND_MARKER));
        writer_metadata_inventory.wait();

        writer_terminal_observation.wait();
        append_event(&writer_path, message(TERMINAL_APPEND_MARKER));
        writer_terminal_observation.wait();

        writer_precommit_physical_revalidation.wait();
        append_event(&writer_path, message(PRECOMMIT_APPEND_MARKER));
        writer_precommit_physical_revalidation.wait();
    });

    let inventory_hook = Arc::clone(&metadata_inventory);
    install_after_codex_metadata_inventory_hook(move || {
        inventory_hook.wait();
        inventory_hook.wait();
    });
    let terminal_hook = Arc::clone(&terminal_observation);
    set_after_jsonl_append_observation_route_binding_hook(parent_path.clone(), move || {
        terminal_hook.wait();
        terminal_hook.wait();
    });
    let precommit_hook = Arc::clone(&precommit_physical_revalidation);
    set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
        precommit_hook.wait();
        precommit_hook.wait();
    });

    let cold_causal = capture_causal_stage();
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    writer.join().expect("bounded Codex appender completed");
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    let cold_sources = causal_by_id(&cold_causal);
    assert_eq!(cold_sources.get(parent).unwrap().counters.cold_sources, 1);
    assert_eq!(cold_sources.get(child).unwrap().counters.cold_sources, 1);

    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 2);
    let cold_generation = initial.generation_id().to_owned();
    let cold_parent = source_snapshot(&initial, parent, "coldprefixuniquetoken");
    let cold_child = source_snapshot(&initial, child, "coldchildstableuniquetoken");
    assert_eq!(cold_parent.search_event_ids.len(), 1);
    assert_eq!(records_for(&initial, parent).len(), 1);
    for marker in [
        INVENTORY_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert!(
            initial
                .search_event_candidates(marker, 8)
                .unwrap()
                .is_empty(),
            "cold publication included deferred suffix {marker}"
        );
    }
    drop(initial);

    let catch_up_causal = capture_causal_stage();
    let caught_up =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(caught_up.failed_routes.is_empty());
    assert!(caught_up.logical_source_failures.is_empty());
    let catch_up_sources = causal_by_id(&catch_up_causal);
    let parent_counters = catch_up_sources.get(parent).unwrap().counters;
    assert_eq!(parent_counters.appended_sources, 1);
    assert_eq!(parent_counters.replaced_sources, 0);
    assert_eq!(parent_counters.scanner_sources_started, 1);
    assert_eq!(parent_counters.scanner_sources_completed, 1);
    assert_eq!(parent_counters.complete_records_scanned, 3);
    assert_eq!(parent_counters.retained_records_scanned, 3);
    assert_eq!(parent_counters.staged_documents, 3);
    assert_exact_zero_work(&catch_up_sources, child, Some(parent));

    let current = VerifiedIndex::open(&index_root).unwrap();
    assert_ne!(current.generation_id(), cold_generation);
    let caught_up_generation = current.generation_id().to_owned();
    let caught_up_parent = source_snapshot(&current, parent, "coldprefixuniquetoken");
    assert_eq!(records_for(&current, parent).len(), 4);
    for marker in [
        INVENTORY_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert_eq!(
            source_snapshot(&current, parent, marker)
                .search_event_ids
                .len(),
            1,
            "catch-up did not index suffix {marker} exactly once"
        );
    }
    assert_eq!(
        source_snapshot(&current, child, "coldchildstableuniquetoken"),
        cold_child
    );
    drop(current);

    let no_op_causal = capture_causal_stage();
    let no_op = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(no_op.failed_routes.is_empty());
    assert!(no_op.logical_source_failures.is_empty());
    let no_op_sources = causal_by_id(&no_op_causal);
    assert_exact_zero_work(&no_op_sources, parent, None);
    assert_exact_zero_work(&no_op_sources, child, Some(parent));

    let terminal = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(terminal.generation_id(), caught_up_generation);
    assert_eq!(
        source_snapshot(&terminal, parent, "coldprefixuniquetoken"),
        caught_up_parent
    );
    for marker in [
        INVENTORY_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert_eq!(
            source_snapshot(&terminal, parent, marker)
                .search_event_ids
                .len(),
            1,
            "terminal no-op changed suffix {marker}"
        );
    }
    assert_eq!(
        source_snapshot(&terminal, child, "coldchildstableuniquetoken"),
        cold_child
    );
}

#[test]
fn destructive_precommit_truncate_and_replacement_preserve_last_good_generation_atomically() {
    // Same-object rewrites are excluded by the Codex append-only provider
    // contract and are covered by the explicit trust-boundary test in shared
    // JSONL. Observable truncation and object replacement must still fail the
    // terminal fence atomically.
    for mutation in ["truncate", "replacement"] {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index_root = temp.path().join("index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fb000-0000-7000-8000-000000000041";
        let path = session_path(&sessions, native_session_id);
        write_session(
            &sessions,
            native_session_id,
            SessionRelationshipKind::Root,
            None,
            [message("lastgooduniquetoken")],
        );
        let registry = register_tree(&[&sessions]);
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        let before = VerifiedIndex::open(&index_root).unwrap();
        let generation = before.generation_id().to_owned();
        let snapshot = source_snapshot(&before, native_session_id, "lastgooduniquetoken");
        drop(before);

        let mutate = path.clone();
        let replacement = path.with_extension("replacement");
        if mutation == "replacement" {
            fs::write(
                &replacement,
                jsonl_bytes([
                    session_meta(native_session_id, SessionRelationshipKind::Root, None),
                    message("replacementuniquetoken"),
                ]),
            )
            .unwrap();
        }
        set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
            destructively_mutate_session(&mutate, &replacement, mutation);
        });
        match refresh_source_backed_generation(&index_root, &registry, writer_options()) {
            Ok(failed) => {
                assert_eq!(failed.failed_routes.len(), 1, "{mutation}");
                assert!(failed.failed_routes[0].carried_forward, "{mutation}");
            }
            Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
                assert_eq!(
                    source.kind,
                    SourceBackedRouteErrorKind::InvalidSource,
                    "{mutation}"
                );
            }
            Err(error) => panic!("unexpected {mutation} failure: {error:?}"),
        }
        let retained = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(retained.generation_id(), generation, "{mutation}");
        assert_eq!(
            source_snapshot(&retained, native_session_id, "lastgooduniquetoken"),
            snapshot,
            "{mutation}"
        );
        assert!(retained
            .search_event_candidates("replacementuniquetoken", 8)
            .unwrap()
            .is_empty());
    }
}
