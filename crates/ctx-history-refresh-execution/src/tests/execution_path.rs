//! Explicit executor-path coverage owned by physical refresh execution.

use super::*;
use ctx_history_capture::{SourceBackedRoute, SourceBackedRouteDriver};
use rusqlite::Connection;

#[test]
fn requested_watch_observations_preserve_present_and_missing_routes() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("history.jsonl");
    std::fs::write(&source_path, b"one\n").unwrap();
    let source = provider_source_for_path(CaptureProvider::OpenCode, source_path.clone());
    let route = SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let present = route.metadata().route_identity.clone().unwrap();
    let missing = SourceRouteIdentity::from_sha256("fe".repeat(32)).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let catalog = registry.watch_catalog();

    let observations = source_backed_requested_route_observations(
        &catalog,
        &BTreeSet::from([present.clone(), missing.clone()]),
    );

    assert_eq!(observations.len(), 2);
    assert!(observations[&present].is_some());
    assert_eq!(observations[&missing], None);

    std::fs::write(source_path, b"one\ntwo\n").unwrap();
    let changed = source_backed_requested_route_observations(
        &catalog,
        &BTreeSet::from([present.clone(), missing.clone()]),
    );
    assert_ne!(changed[&present], observations[&present]);
    assert_eq!(changed[&missing], None);
    assert_eq!(
        changed.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([present, missing])
    );
}

#[test]
fn provider_wide_execution_discovers_once_and_preserves_progress_order() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(home.join(".forge")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let forge = home.join(".forge/.forge.db");
    let forge_writer = Connection::open(&forge).unwrap();
    forge_writer
        .pragma_update(None, "journal_mode", "wal")
        .unwrap();
    forge_writer
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    forge_writer
        .execute_batch("create table conversations (id text primary key);")
        .unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let updates = std::sync::Mutex::new(Vec::new());
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        updates.lock().unwrap().push((
            update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
            update.completed_records,
            update.completed_bytes,
            update.providers,
            update.processed_sessions,
            update.processed_messages,
            update.processed_tool_calls,
            update.processed_bytes,
            update.elapsed_millis,
        ));
        Ok(())
    };
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "all-provider-request",
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::All,
        BTreeSet::new(),
        SourceBackedRefreshCoveredPublication::default(),
        &discovery,
        &TestPublishedState,
        &report_progress,
    );
    let mut provider_wide_calls = 0;

    let publication = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        None,
        |observed_discovery,
         observed_report,
         observed_discovery_duration,
         observed_request_id,
         observed_operation,
         observed_data_root,
         observed_index_root,
         observed_explicit_source_catalog,
         observed_scope,
         observed_covered_route_ids,
         observed_covered_publication,
         _observed_published_state,
         progress| {
            provider_wide_calls += 1;
            assert_eq!(observed_discovery.home(), discovery.home());
            assert_eq!(observed_discovery.cwd(), discovery.cwd());
            assert_eq!(observed_discovery.data_root(), Some(data_root.as_path()));
            assert!(observed_report.sources.iter().any(|source| {
                source.provider == CaptureProvider::ForgeCode
                    && source.path == forge
                    && source.status == ProviderSourceStatus::Available
            }));
            assert_ne!(observed_discovery_duration, StdDuration::ZERO);
            assert_eq!(observed_request_id, "all-provider-request");
            assert_eq!(observed_operation, RefreshOperation::Refresh);
            assert_eq!(observed_data_root, data_root);
            assert_eq!(observed_index_root, index_root);
            assert!(observed_explicit_source_catalog.is_none());
            assert_eq!(observed_scope, SourceBackedRefreshScope::All);
            assert!(observed_covered_route_ids.is_empty());
            assert!(observed_covered_publication.route_results.is_empty());
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "discovering",
                    completed_sources: 0,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    elapsed: StdDuration::from_secs(1),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: 1,
                    total_sources: 2,
                    current_source: Some("provider-wide-route".to_owned()),
                    completed_records: Some(11),
                    completed_bytes: Some(4_096),
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    processed_sessions: 3,
                    processed_messages: 8,
                    processed_tool_calls: 3,
                    processed_bytes: 4_096,
                    elapsed: StdDuration::from_millis(2_500),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: ctx_history_capture::SourceBackedRefreshProgress {
                    phase: "verifying",
                    completed_sources: 2,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    providers: vec![CaptureProvider::Codex, CaptureProvider::Claude],
                    processed_sessions: 3,
                    processed_messages: 8,
                    processed_tool_calls: 3,
                    processed_bytes: 4_096,
                    elapsed: StdDuration::from_secs(3),
                    ..Default::default()
                },
                current_source_progress: None,
                exact_scan_progress: None,
            })?;
            Ok(test_publication("all-provider-generation"))
        },
    )
    .unwrap();
    drop(forge_writer);

    assert_eq!(provider_wide_calls, 1);
    assert_eq!(publication.generation_id, "all-provider-generation");
    assert_eq!(
        updates.into_inner().unwrap(),
        vec![
            (
                "discovering".to_owned(),
                0,
                2,
                None,
                None,
                None,
                vec!["codex".to_owned(), "claude".to_owned()],
                0,
                0,
                0,
                0,
                Some(1_000),
            ),
            (
                "refreshing".to_owned(),
                1,
                2,
                Some("provider-wide-route".to_owned()),
                Some(11),
                Some(4_096),
                vec!["codex".to_owned(), "claude".to_owned()],
                3,
                8,
                3,
                4_096,
                Some(2_500),
            ),
            (
                "verifying".to_owned(),
                2,
                2,
                None,
                None,
                None,
                vec!["codex".to_owned(), "claude".to_owned()],
                3,
                8,
                3,
                4_096,
                Some(3_000),
            ),
        ]
    );
}

#[test]
fn exact_execution_without_admitted_authority_cannot_fall_back_to_global_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (home, cwd, discovery) = discovery_fixture(temp.path());
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let route = SourceRouteIdentity::from_sha256("ac".repeat(32)).unwrap();
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "missing-scoped-authority",
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::exact([route]),
        BTreeSet::new(),
        SourceBackedRefreshCoveredPublication::default(),
        &discovery,
        &TestPublishedState,
        &progress,
    )
    .with_admitted_discovery_requirement(true);

    let error = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        None,
        |_, _, _, _, _, _, _, _, _, _, _, _, _| {
            panic!("exact execution reached refresh after global fallback")
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}")
        .contains("selected source refresh has no admitted discovery authority"));
}

#[test]
fn warm_exact_carries_unselected_routes_while_receipt_stays_selected() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());

    let codex_root = temp.path().join("codex-sessions");
    fs::create_dir_all(&codex_root).unwrap();
    let codex_session = codex_root.join("rollout.jsonl");
    fs::write(
        &codex_session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000701",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/exact-carry",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "codex warm"}]
                }
            })
        ),
    )
    .unwrap();
    let claude_root = temp.path().join("claude-projects");
    let claude_session = claude_root.join("project/session.jsonl");
    fs::create_dir_all(claude_session.parent().unwrap()).unwrap();
    fs::write(
        &claude_session,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "literal-claude-warm",
                "sessionId": "019fb700-0000-7000-8000-000000000702",
                "message": {"role": "user", "content": "claude warm"}
            })
        ),
    )
    .unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, codex_root);
    let claude_source = provider_source_for_path(CaptureProvider::Claude, claude_root);
    let codex_route = automatic_source_backed_route_identity(&codex_source).unwrap();
    let claude_route = automatic_source_backed_route_identity(&claude_source).unwrap();
    let report = DiscoveryReport {
        sources: vec![codex_source.clone(), claude_source],
        issues: Vec::new(),
    };
    let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    let cold = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(cold.route_results.len(), 2);

    fs::write(
        &codex_session,
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-17T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fb700-0000-7000-8000-000000000701",
                    "timestamp": "2026-08-17T00:00:00Z",
                    "cwd": "/repo/exact-carry",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-17T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "codex exact"}]
                }
            })
        ),
    )
    .unwrap();
    let exact_routes = BTreeSet::from([codex_route.clone()]);
    let mut exact_progress = |_: CaptureSourceBackedDetailedRefreshProgress| Ok(());
    let exact = refresh_all_provider_sources_route_local(
        &discovery,
        DiscoveryReport {
            sources: vec![codex_source],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        "warm-exact-carry",
        RefreshOperation::Refresh,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::Exact(exact_routes.clone()),
        &BTreeSet::new(),
        &SourceBackedRefreshCoveredPublication::default(),
        &TestPublishedState,
        &mut exact_progress,
    )
    .unwrap();

    assert_eq!(exact.route_results.len(), 1);
    assert_eq!(exact.route_results[0].route_identity, codex_route.as_str());
    let published = VerifiedIndex::open(&index_root).unwrap();
    assert!(published.manifest().source_route(&codex_route).is_some());
    assert!(published.manifest().source_route(&claude_route).is_some());
}
