use super::*;

#[test]
fn provider_checkpoint_stays_bounded_across_terminal_authority_lifecycles() {
    const MAX_AUTHORITY_ENTRIES: usize = 256;
    const MAX_FRONTIER_ENVELOPE_BYTES: usize = 64 * 1024;

    let temp = crate::test_support_paths::tempdir().unwrap();

    let exact_sessions = temp.path().join("exact-sessions");
    let exact_index = temp.path().join("exact-index");
    fs::create_dir_all(&exact_sessions).unwrap();
    let exact_owner = "checkpoint-envelope-exact";
    write_session(
        &exact_sessions,
        exact_owner,
        SessionRelationshipKind::Root,
        None,
        terminal_authority_events(MAX_AUTHORITY_ENTRIES),
    );
    let exact_registry = register_tree(&[&exact_sessions]);
    let exact_receipt =
        refresh_source_backed_generation(&exact_index, &exact_registry, writer_options()).unwrap();
    assert!(
        exact_receipt.failed_routes.is_empty(),
        "exact authority boundary failed publication: {:?}",
        exact_receipt.failed_routes
    );
    assert!(exact_receipt.logical_source_failures.is_empty());
    let exact = VerifiedIndex::open(&exact_index).unwrap();
    let (exact_semantic_bytes, exact_family_bytes, exact_frontier_bytes, exact_checkpoint) =
        provider_checkpoint_envelope(&exact, exact_owner);
    assert!(exact_family_bytes + 5 <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(exact_frontier_bytes <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(exact_semantic_bytes > 0);
    assert_current_provider_checkpoint(&exact_checkpoint);
    let exact_checkpoint_json = serde_json::to_string(&exact_checkpoint).unwrap();
    assert!(!exact_checkpoint_json.contains("event-body-secret-must-not-reach-checkpoint"));
    drop(exact);

    append_event(
        &session_path(&exact_sessions, exact_owner),
        checkpoint_mcp_terminal("mcp-checkpoint-suffix-exhaustion"),
    );
    let suffix_exhausted = capture_causal_stage();
    refresh_source_backed_generation(&exact_index, &exact_registry, writer_options()).unwrap();
    let suffix_exhausted_counters = causal_by_id(&suffix_exhausted)
        .get(exact_owner)
        .unwrap()
        .counters;
    assert_eq!(suffix_exhausted_counters.appended_sources, 0);
    assert_eq!(suffix_exhausted_counters.replaced_sources, 1);
    let suffix_exhausted = VerifiedIndex::open(&exact_index).unwrap();
    let (_, _, _, suffix_exhausted_checkpoint) =
        provider_checkpoint_envelope(&suffix_exhausted, exact_owner);
    assert_current_provider_checkpoint(&suffix_exhausted_checkpoint);
    drop(suffix_exhausted);

    let exhausted_sessions = temp.path().join("exhausted-sessions");
    let exhausted_index = temp.path().join("exhausted-index");
    fs::create_dir_all(&exhausted_sessions).unwrap();
    let exhausted_owner = "checkpoint-envelope-exhausted";
    write_session(
        &exhausted_sessions,
        exhausted_owner,
        SessionRelationshipKind::Root,
        None,
        terminal_authority_events(MAX_AUTHORITY_ENTRIES + 1),
    );
    let exhausted_registry = register_tree(&[&exhausted_sessions]);
    let exhausted_receipt =
        refresh_source_backed_generation(&exhausted_index, &exhausted_registry, writer_options())
            .unwrap();
    assert!(
        exhausted_receipt.failed_routes.is_empty(),
        "exhausted authority boundary failed publication: {:?}",
        exhausted_receipt.failed_routes
    );
    assert!(exhausted_receipt.logical_source_failures.is_empty());
    let exhausted = VerifiedIndex::open(&exhausted_index).unwrap();
    let (
        exhausted_semantic_bytes,
        exhausted_family_bytes,
        exhausted_frontier_bytes,
        exhausted_checkpoint,
    ) = provider_checkpoint_envelope(&exhausted, exhausted_owner);
    assert!(exhausted_family_bytes + 5 <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(exhausted_frontier_bytes <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(exhausted_semantic_bytes > 0);
    assert_current_provider_checkpoint(&exhausted_checkpoint);
    drop(exhausted);

    append_event(
        &session_path(&exhausted_sessions, exhausted_owner),
        checkpoint_mcp_terminal("mcp-checkpoint-after-exhaustion"),
    );
    let exhausted_append_observed = capture_causal_stage();
    let appended_receipt =
        refresh_source_backed_generation(&exhausted_index, &exhausted_registry, writer_options())
            .unwrap();
    assert!(appended_receipt.failed_routes.is_empty());
    assert!(appended_receipt.logical_source_failures.is_empty());
    let exhausted_append_counters = causal_by_id(&exhausted_append_observed)
        .get(exhausted_owner)
        .unwrap()
        .counters;
    assert_eq!(exhausted_append_counters.appended_sources, 1);
    assert_eq!(exhausted_append_counters.replaced_sources, 0);
    let appended = VerifiedIndex::open(&exhausted_index).unwrap();
    let (
        appended_semantic_bytes,
        appended_family_bytes,
        appended_frontier_bytes,
        appended_checkpoint,
    ) = provider_checkpoint_envelope(&appended, exhausted_owner);
    assert!(appended_family_bytes + 5 <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(appended_frontier_bytes <= MAX_FRONTIER_ENVELOPE_BYTES);
    assert!(appended_semantic_bytes > 0);
    assert_current_provider_checkpoint(&appended_checkpoint);
    eprintln!(
        "Codex checkpoint envelopes: exact256 semantic={exact_semantic_bytes} family={exact_family_bytes} frontier={exact_frontier_bytes}; exhausted257 semantic={exhausted_semantic_bytes} family={exhausted_family_bytes} frontier={exhausted_frontier_bytes}; exhausted_append semantic={appended_semantic_bytes} family={appended_family_bytes} frontier={appended_frontier_bytes}"
    );
}

#[test]
fn parent_lifecycle_never_opens_scans_or_replaces_unchanged_descendants() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let parent = "019fb000-0000-7000-8000-000000000001";
    let child = "019fb000-0000-7000-8000-000000000002";
    let grandchild = "019fb000-0000-7000-8000-000000000003";
    let great_grandchild = "019fb000-0000-7000-8000-000000000004";
    let parent_path = session_path(&sessions, parent);

    write_session_with_payload_session_id(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        parent,
        [message("child-stable-marker")],
    );
    write_session_with_payload_session_id(
        &sessions,
        grandchild,
        SessionRelationshipKind::Delegated,
        Some(child),
        parent,
        [message("grandchild-stable-marker")],
    );
    write_session_with_payload_session_id(
        &sessions,
        great_grandchild,
        SessionRelationshipKind::Delegated,
        Some(grandchild),
        parent,
        [message("great-grandchild-stable-marker")],
    );
    let registry = register_tree(&[&sessions]);
    let initial_receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial_receipt.failed_routes.is_empty());
    assert!(initial_receipt.logical_source_failures.is_empty());
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 3);
    let child_snapshot = source_snapshot(&initial, child, "child-stable-marker");
    let grandchild_snapshot = source_snapshot(&initial, grandchild, "grandchild-stable-marker");
    let great_grandchild_snapshot =
        source_snapshot(&initial, great_grandchild, "great-grandchild-stable-marker");
    let child_records = records_for(&initial, child);
    let grandchild_records = records_for(&initial, grandchild);
    let great_grandchild_records = records_for(&initial, great_grandchild);
    assert!(child_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(child)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    assert!(grandchild_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(grandchild)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    assert!(great_grandchild_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(great_grandchild)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    drop(initial);
    let descendants = [
        (child, parent),
        (grandchild, child),
        (great_grandchild, grandchild),
    ];

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-initial-marker")],
    );
    let arrived = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(arrived.get(parent).unwrap().counters.cold_sources, 1);

    append_event(&parent_path, message("parent-append-marker"));
    let appended = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    let parent_append = &appended.get(parent).unwrap().counters;
    assert_eq!(parent_append.appended_sources, 1);
    assert_eq!(parent_append.scanner_sources_started, 1);

    replace_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message(&format!(
            "parent-rewrite-marker-{}",
            "x".repeat(1_024)
        ))],
    );
    let rewritten = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(rewritten.get(parent).unwrap().counters.replaced_sources, 1);

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-truncated")],
    );
    let truncated = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(truncated.get(parent).unwrap().counters.replaced_sources, 1);

    fs::remove_file(&parent_path).unwrap();
    let deleted = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert!(!deleted.contains_key(parent));

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-reappeared-marker")],
    );
    let reappeared = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(reappeared.get(parent).unwrap().counters.cold_sources, 1);

    let final_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        source_snapshot(&final_index, child, "child-stable-marker"),
        child_snapshot
    );
    assert_eq!(
        source_snapshot(&final_index, grandchild, "grandchild-stable-marker"),
        grandchild_snapshot
    );
    assert_eq!(
        source_snapshot(
            &final_index,
            great_grandchild,
            "great-grandchild-stable-marker"
        ),
        great_grandchild_snapshot
    );
}

#[test]
fn nested_payload_session_id_is_ignored_and_changed_child_processes_only_itself() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let root = "019fb000-0000-7000-8000-000000000005";
    let parent = "019fb000-0000-7000-8000-000000000006";
    let child = "019fb000-0000-7000-8000-000000000007";
    write_session(
        &sessions,
        root,
        SessionRelationshipKind::Root,
        None,
        [message("nestedrootuniquetokenaaa")],
    );
    write_session_with_payload_session_id(
        &sessions,
        parent,
        SessionRelationshipKind::Delegated,
        Some(root),
        root,
        [message("nestedparentuniquetokenbbb")],
    );
    write_session_with_payload_session_id(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        root,
        [message("nestedchildinitialuniquetokenccc")],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 3);
    assert_eq!(
        initial
            .search_event_candidates("nestedchildinitialuniquetokenccc", 8)
            .unwrap()
            .len(),
        1
    );
    let parent_session_id = records_for(&initial, parent)[0].session_id;
    let child_records = records_for(&initial, child);
    assert!(child_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(child)
            && record.parent_session_id == Some(parent_session_id)
            && record.root_session_id == parent_session_id
    }));
    drop(initial);

    replace_session_with_payload_session_id(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        root,
        [message("nestedchildrewrittenuniquetokenddd")],
    );
    let observed = capture_causal_stage();
    let rewritten =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(rewritten.failed_routes.is_empty());
    assert!(rewritten.logical_source_failures.is_empty());
    let sources = causal_by_id(&observed);
    assert_exact_zero_work(&sources, root, None);
    assert_exact_zero_work(&sources, parent, Some(root));
    let child_counters = sources.get(child).unwrap().counters;
    assert_eq!(child_counters.scanner_source_opens, 1);
    assert_eq!(child_counters.scanner_sources_started, 1);
    assert_eq!(child_counters.scanner_sources_completed, 1);
    assert!(child_counters.scanner_bytes_read > 0);
    assert!(child_counters.typed_json_parses > 0);
    assert_eq!(child_counters.replaced_sources, 1);
    assert_eq!(child_counters.writer_mutated_sources, 1);
    assert_eq!(
        sources
            .iter()
            .filter(|(_, source)| source.counters.scanner_sources_started != 0)
            .map(|(native_session_id, _)| native_session_id.as_str())
            .collect::<Vec<_>>(),
        vec![child]
    );

    let current = VerifiedIndex::open(&index_root).unwrap();
    assert!(current
        .search_event_candidates("nestedchildinitialuniquetokenccc", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        current
            .search_event_candidates("nestedchildrewrittenuniquetokenddd", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(records_for(&current, child).iter().any(|record| {
        record
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| body.contains("nestedchildrewrittenuniquetokenddd"))
    }));
}

#[test]
fn append_after_large_terminal_authority_prefix_replays_combined_authority_once() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-000000000009";
    let path = session_path(&sessions, native_session_id);
    let mut events = (0..4_097)
        .map(|index| {
            exec_result(
                &format!("completed-prefix-call-{index}"),
                "completed-prefix-result",
            )
        })
        .collect::<Vec<_>>();
    events.push(message("large-prefix-seed"));
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
    let (_, _, _, initial_checkpoint) = provider_checkpoint_envelope(&initial, native_session_id);
    assert_current_provider_checkpoint(&initial_checkpoint);
    drop(initial);

    append_event(&path, message("largeprefixappenduniquetoken"));
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    let counters = sources.get(native_session_id).unwrap().counters;
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    assert_eq!(counters.scanner_sources_started, 1);
    assert_eq!(counters.complete_records_scanned, 1);
    assert_eq!(counters.retained_records_scanned, 1);
    assert_eq!(counters.staged_documents, 1);
    assert!(counters.mcp_terminal_authority_bytes_read < 4 * 1024);
    assert!(counters.scanner_bytes_read < 4 * 1024);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    let appended_event_ids = appended
        .search_event_candidates("largeprefixappenduniquetoken", 8)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(appended_event_ids.len(), 1);

    let cold_index_root = temp.path().join("cold-index");
    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&cold, native_session_id)).unwrap(),
        appended_certificate
    );
    assert_eq!(
        cold.search_event_candidates("largeprefixappenduniquetoken", 8)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        appended_event_ids
    );
}

#[test]
fn pending_prefix_call_is_restored_and_completed_by_append_suffix() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-00000000000a";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [exec_call("pending-prefix-call")],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(
        &path,
        exec_result("pending-prefix-call", "pendingprefixcompletedbysuffix"),
    );
    let observed = capture_causal_stage();
    let appended =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        verified
            .search_event_candidates("pendingprefixcompletedbysuffix", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn replayed_source_state_is_exact_across_cold_unchanged_and_child_mcp_append() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let command = "git commit -m exact && git rev-parse HEAD";
    let parent = "019fb000-0000-7000-8000-00000000004b";
    let child = "019fb000-0000-7000-8000-00000000004c";
    let path = session_path(&sessions, child);
    let mut metadata = session_meta(child, SessionRelationshipKind::Forked, Some(parent));
    metadata["payload"]
        .as_object_mut()
        .unwrap()
        .remove("cli_version");
    fs::write(
        &path,
        jsonl_bytes([
            metadata,
            unrelated_tool_call("replayed-child-mcp-call"),
            exec_call_in("replayed-child-copied-call", command, &repository),
        ]),
    )
    .unwrap();
    let registry = register_tree(&[&sessions]);

    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&index_root).unwrap();
    let cold_snapshot = source_snapshot(&cold, child, "replayed-child-mcp-call");
    let (_, _, _, cold_checkpoint) = provider_checkpoint_envelope(&cold, child);
    assert_current_provider_checkpoint(&cold_checkpoint);
    drop(cold);

    let unchanged_observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let unchanged_sources = causal_by_id(&unchanged_observed);
    assert_exact_zero_work(&unchanged_sources, child, Some(parent));
    let unchanged = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        source_snapshot(&unchanged, child, "replayed-child-mcp-call"),
        cold_snapshot
    );
    drop(unchanged);

    append_event(
        &path,
        mcp_terminal(
            "replayed-child-mcp-call",
            "replayed-child-server",
            "replayedchildmcpattributiontoken",
        ),
    );
    append_event(
        &path,
        successful_result(
            "replayed-child-copied-call",
            format!("replayedchildcopiedorigintoken\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    let append_observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let append_sources = causal_by_id(&append_observed);
    assert_eq!(
        append_sources.get(child).unwrap().counters.appended_sources,
        1
    );
    assert_eq!(
        append_sources.get(child).unwrap().counters.replaced_sources,
        0
    );
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_records = records_for(&appended, child);
    let terminal = appended_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("replayedchildmcpattributiontoken"))
        })
        .expect("replayed child MCP terminal record");
    assert!(terminal.mcp_tool_call.is_some());
    let copied = appended_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("replayedchildcopiedorigintoken"))
        })
        .expect("replayed child copied result record");
    assert!(
        matches!(copied.event_origin, EventOrigin::CopiedFromAncestor { .. }),
        "unexpected copied result: {copied:#?}"
    );
    let appended_snapshot = source_snapshot(&appended, child, "replayedchildmcpattributiontoken");
    let (_, _, _, appended_checkpoint) = provider_checkpoint_envelope(&appended, child);
    assert_current_provider_checkpoint(&appended_checkpoint);
    drop(appended);

    let cold_final_root = temp.path().join("cold-final-index");
    refresh_source_backed_generation(&cold_final_root, &registry, writer_options()).unwrap();
    let cold_final = VerifiedIndex::open(&cold_final_root).unwrap();
    assert_eq!(
        source_snapshot(&cold_final, child, "replayedchildmcpattributiontoken"),
        appended_snapshot
    );
}

#[test]
fn suffix_completes_last_of_twenty_four_replayed_pending_calls() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-00000000004a";
    let path = session_path(&sessions, native_session_id);
    let pending = (0..24)
        .map(|index| exec_call(&format!("replayed-pending-{index:02}")))
        .collect::<Vec<_>>();
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        pending,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    let (_, _, _, initial_checkpoint) = provider_checkpoint_envelope(&initial, native_session_id);
    assert_current_provider_checkpoint(&initial_checkpoint);
    drop(initial);

    append_event(
        &path,
        exec_result("replayed-pending-23", "twentyfourthpendingcontexttoken"),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_snapshot = source_snapshot(
        &appended,
        native_session_id,
        "twentyfourthpendingcontexttoken",
    );
    let result = records_for(&appended, native_session_id)
        .into_iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("twentyfourthpendingcontexttoken"))
        })
        .unwrap();
    assert_eq!(result.event_type, "command_output");
    let (_, _, _, checkpoint) = provider_checkpoint_envelope(&appended, native_session_id);
    assert_current_provider_checkpoint(&checkpoint);
    drop(appended);

    let cold_root = temp.path().join("cold-final-index");
    refresh_source_backed_generation(&cold_root, &registry, writer_options()).unwrap();
    assert_eq!(
        source_snapshot(
            &VerifiedIndex::open(&cold_root).unwrap(),
            native_session_id,
            "twentyfourthpendingcontexttoken",
        ),
        appended_snapshot
    );
}

#[test]
fn retired_semantic_v2_checkpoint_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("retiredv2", retired_semantic_v2_checkpoint);
}

#[test]
fn malformed_semantic_checkpoint_key_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("malformedkey", |_| TypedKey::U64(2));
}

#[test]
fn terminal_nul_checkpoint_forces_replacement_and_binds_full_admitted_revision() {
    let (_temp, sessions, index_root) = codex_test_workspace();
    let native_session_id = "019fb000-0000-7000-8000-00000000000b";
    let path = session_path(&sessions, native_session_id);
    let mut initial = jsonl_bytes([
        session_meta(native_session_id, SessionRelationshipKind::Root, None),
        message("terminal-nul-initial"),
    ]);
    initial.resize(initial.len() + 4 * 1024, 0);
    fs::write(&path, &initial).unwrap();
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial_index = VerifiedIndex::open(&index_root).unwrap();
    let initial_certificate = certificate_for(&initial_index, native_session_id);
    let initial_frontier = initial_certificate.frontier().unwrap();
    assert_eq!(
        initial_frontier.certified_prefix_bytes(),
        initial.len() as u64
    );
    assert_eq!(
        *initial_frontier.certified_prefix_digest(),
        jsonl_prefix_digest(&initial)
    );
    assert_eq!(
        checkpoint_admitted_revision_for_test(&initial_certificate).unwrap(),
        (Some(Sha256::digest(&initial).into()), true)
    );
    drop(initial_index);

    append_event(&path, message("terminal-nul-after-boundary"));
    let appended_bytes = fs::read(&path).unwrap();
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 0);
    assert_eq!(counters.replaced_sources, 1);
    let replaced = VerifiedIndex::open(&index_root).unwrap();
    let replaced_certificate = certificate_for(&replaced, native_session_id);
    let replaced_frontier = replaced_certificate.frontier().unwrap();
    assert_eq!(
        replaced_frontier.certified_prefix_bytes(),
        appended_bytes.len() as u64
    );
    assert_eq!(
        *replaced_frontier.certified_prefix_digest(),
        jsonl_prefix_digest(&appended_bytes)
    );
    assert_eq!(
        checkpoint_admitted_revision_for_test(&replaced_certificate).unwrap(),
        (Some(Sha256::digest(&appended_bytes).into()), false)
    );
    drop(replaced);

    let mut rewritten = jsonl_bytes([
        session_meta(native_session_id, SessionRelationshipKind::Root, None),
        message("terminal-nul-rewrite-visible"),
    ]);
    rewritten.resize(appended_bytes.len(), 0);
    fs::write(&path, &rewritten).unwrap();
    let rewritten_receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(
        rewritten_receipt.failed_routes.is_empty(),
        "unexpected rewrite failures: {:?}",
        rewritten_receipt.failed_routes
    );
    let rewritten_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        rewritten_index
            .search_event_candidates("terminal-nul-rewrite-visible", 8)
            .unwrap()
            .len(),
        1
    );
    let rewritten_certificate = certificate_for(&rewritten_index, native_session_id);
    let rewritten_frontier = rewritten_certificate.frontier().unwrap();
    assert_eq!(
        *rewritten_frontier.certified_prefix_digest(),
        jsonl_prefix_digest(&rewritten)
    );
    assert_eq!(
        checkpoint_admitted_revision_for_test(&rewritten_certificate).unwrap(),
        (Some(Sha256::digest(&rewritten).into()), true)
    );
}

#[test]
fn selected_routes_process_child_only_and_never_abort_for_unselected_descendants() {
    let temp = tempdir().unwrap();
    let parent_root = temp.path().join("parent-sessions");
    let child_root = temp.path().join("child-sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&parent_root).unwrap();
    fs::create_dir_all(&child_root).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000011";
    let child = "019fb000-0000-7000-8000-000000000012";
    let parent_path = session_path(&parent_root, parent);
    let child_path = session_path(&child_root, child);
    write_session(
        &parent_root,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("selected-parent-initial")],
    );
    write_session(
        &child_root,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("selected-child-initial")],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut registry, &parent_path);
    add_explicit_route(&mut registry, &child_path);
    let parent_route = route_identity(&registry, &parent_path);
    let child_route = route_identity(&registry, &child_path);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&child_path, message("child-only-selected-marker"));
    let child_observed = capture_causal_stage();
    refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [child_route.clone()],
    )
    .unwrap();
    let child_sources = causal_by_id(&child_observed);
    assert_eq!(child_sources.len(), 1);
    assert_eq!(
        child_sources.get(child).unwrap().counters.appended_sources,
        1
    );
    assert!(!child_sources.contains_key(parent));

    append_event(&parent_path, message("simultaneousparentuniquetoken"));
    append_event(&child_path, message("simultaneouschilduniquetoken"));
    let before_unselected_child = source_snapshot(
        &VerifiedIndex::open(&index_root).unwrap(),
        child,
        "child-only-selected-marker",
    );
    let parent_observed = capture_causal_stage();
    let selected_parent = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [parent_route],
    )
    .unwrap();
    assert!(selected_parent.failed_routes.is_empty());
    let parent_sources = causal_by_id(&parent_observed);
    assert_eq!(parent_sources.len(), 1);
    assert_eq!(
        parent_sources
            .get(parent)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    assert!(!parent_sources.contains_key(child));
    let after_parent = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        source_snapshot(&after_parent, child, "child-only-selected-marker"),
        before_unselected_child
    );
    assert!(after_parent
        .search_event_candidates("simultaneouschilduniquetoken", 8)
        .unwrap()
        .is_empty());

    let child_catchup = capture_causal_stage();
    refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [child_route],
    )
    .unwrap();
    let catchup_sources = causal_by_id(&child_catchup);
    assert_eq!(
        catchup_sources
            .get(child)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    let caught_up = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        caught_up
            .search_event_candidates("simultaneousparentuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        caught_up
            .search_event_candidates("simultaneouschilduniquetoken", 8)
            .unwrap()
            .len(),
        1
    );

    append_event(&parent_path, message("bothselectedparentuniquetoken"));
    append_event(&child_path, message("bothselectedchilduniquetoken"));
    let both_selected =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(both_selected.failed_routes.is_empty());
    let simultaneous = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        simultaneous
            .search_event_candidates("bothselectedparentuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        simultaneous
            .search_event_candidates("bothselectedchilduniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn overlapping_automatic_and_explicit_routes_keep_selected_generation_ownership() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000013";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("automatic-explicit-cold-marker")],
    );

    let mut registry = register_tree(&[&sessions]);
    add_explicit_route(&mut registry, &path);
    let automatic_route = route_identity(&registry, &sessions);
    let explicit_route = route_identity(&registry, &path);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("automatic-explicit-cold-marker", 8)
            .unwrap()
            .len(),
        1
    );

    append_event(&path, message("explicit-only-append-marker"));
    let explicit = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [explicit_route],
    )
    .unwrap();
    assert!(explicit.failed_routes.is_empty());
    assert_eq!(explicit.sources.len(), 1);

    append_event(&path, message("automatic-only-append-marker"));
    let automatic = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [automatic_route],
    )
    .unwrap();
    assert!(automatic.failed_routes.is_empty());
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    for marker in [
        "automatic-explicit-cold-marker",
        "explicit-only-append-marker",
        "automatic-only-append-marker",
    ] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record.content.normalized_body.as_deref() == Some(marker))
                .count(),
            1,
            "missing or duplicated {marker}"
        );
    }
}

#[test]
fn fork_invocation_boundary_separates_copied_and_unique_exact_outcomes() {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let command = "git commit -m exact && git rev-parse HEAD";
    let parent = "019fb000-0000-7000-8000-000000000021";
    let child = "019fb000-0000-7000-8000-000000000022";
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call_in("copied-call", command, &repository),
            successful_result(
                "copied-call",
                format!("positive-copied-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            message("origin-parent-marker"),
        ],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Forked,
        Some(parent),
        [
            turn_context_with_id("019fa000-0000-7000-8000-000000000001"),
            exec_call_in("copied-call", command, &repository),
            turn_context(),
            successful_result(
                "copied-call",
                format!("positive-copied-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("local-call", command, &repository),
            successful_result(
                "local-call",
                format!("unique-local-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = VerifiedIndex::open(&index_root).unwrap();
    let child_snapshot = source_snapshot(&before, child, "result-marker");
    let records = records_for(&before, child);
    assert!(records.iter().all(|record| {
        record.session_relationship == SessionRelationshipKind::Forked
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    let copied = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("positive-copied-result-marker"))
        })
        .expect("copied result record");
    assert!(matches!(
        copied.event_origin,
        EventOrigin::CopiedFromAncestor { .. }
    ));
    assert!(copied.repository_vcs_observations.is_empty());
    assert!(copied.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("copied_provider_history_has_ancestor_execution")
    }));
    let local = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("unique-local-result-marker"))
        })
        .expect("local result record");
    assert_eq!(local.event_origin, EventOrigin::UniqueToSession);
    assert!(local
        .repository_vcs_observations
        .iter()
        .any(|observation| matches!(
            &observation.kind,
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                    && outcome.produced_object_ids.iter().any(|object_id| object_id.hex == oid)
        )));
    assert!(!local.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));

    append_event(
        &session_path(&sessions, parent),
        message("origin-parent-append"),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_exact_zero_work(&sources, child, Some(parent));
    assert_eq!(
        source_snapshot(
            &VerifiedIndex::open(&index_root).unwrap(),
            child,
            "result-marker"
        ),
        child_snapshot
    );

    append_event(
        &session_path(&sessions, child),
        message("child-append-after-restart-marker"),
    );
    let child_append = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let child_append_sources = causal_by_id(&child_append);
    assert_eq!(
        child_append_sources
            .get(child)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    let restarted_records = records_for(&VerifiedIndex::open(&index_root).unwrap(), child);
    let restarted_copied = restarted_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("positive-copied-result-marker"))
        })
        .unwrap();
    assert!(matches!(
        restarted_copied.event_origin,
        EventOrigin::CopiedFromAncestor { .. }
    ));
}

#[test]
fn root_owned_exact_commit_and_pr_203_share_certified_repository_origin() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    assert!(Command::new("git")
        .args([
            "remote",
            "set-url",
            "origin",
            "https://github.com/ctxrs/ctx.git",
        ])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    let oid = repository_head(&repository);
    let native_session_id = "019fb000-0000-7000-8000-000000000024";
    let pr_command = concat!(
        "git push -u origin codex/exact-repository-origin\n",
        "gh pr create --base main --head codex/exact-repository-origin ",
        "--title 'exact repository origin' --body 'exact repository origin'"
    );
    let unrelated_terminals = (0..257).map(|index| {
        serde_json::json!({
            "timestamp": "2026-08-09T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": format!("unrelated-terminal-{index}")
            }
        })
    });
    let exact_events = [
        exec_call_in(
            "root-commit",
            "git commit -m exact-root-commit && git rev-parse HEAD",
            &repository,
        ),
        successful_result(
            "root-commit",
            format!("[main abc1234] exact-root-commit\n{oid}\n"),
        ),
        exec_call_in("root-pr-203", pr_command, &repository),
        successful_result(
            "root-pr-203",
            "To https://github.com/ctxrs/ctx.git\nhttps://github.com/ctxrs/ctx/pull/203\n"
                .to_owned(),
        ),
    ];
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        unrelated_terminals.chain(exact_events),
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&verified, native_session_id);
    let commit = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("exact-root-commit"))
                && !record.repository_vcs_observations.is_empty()
        })
        .expect("root exact commit record");
    assert_eq!(commit.event_origin, EventOrigin::UniqueToSession);
    let commit_outcome = commit
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit =>
            {
                Some((observation, outcome.as_ref()))
            }
            _ => None,
        })
        .expect("certified exact commit outcome");
    assert!(commit_outcome
        .1
        .produced_object_ids
        .iter()
        .any(|object_id| object_id.hex == oid));

    let pull_request = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("https://github.com/ctxrs/ctx/pull/203"))
        })
        .expect("root PR #203 record");
    assert_eq!(pull_request.event_origin, EventOrigin::UniqueToSession);
    let (pr_observation, pr_outcome) = pull_request
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::PullRequestCreated =>
            {
                Some((observation, outcome.as_ref()))
            }
            _ => None,
        })
        .expect("certified PR #203 creation outcome");
    let pr_identity = pr_outcome.pull_request.as_ref().expect("PR identity");
    assert_eq!(pr_identity.number, 203);
    let binding = pull_request
        .repository_bindings
        .iter()
        .find(|binding| binding.binding_id == pr_observation.repository_binding_id)
        .expect("PR #203 repository binding");
    assert!(binding.accepts_pull_request(pr_identity));
    assert_eq!(
        commit_outcome.0.repository_binding_id,
        pr_observation.repository_binding_id
    );
    drop(verified);

    let path = session_path(&sessions, native_session_id);
    append_event(&path, message("repository-authority-unrelated-append"));
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let counters = causal_by_id(&observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    assert!(records_for(&appended, native_session_id)
        .iter()
        .any(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("exact-root-commit"))
                && !record.repository_vcs_observations.is_empty()
        }));
}
