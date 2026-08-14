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
