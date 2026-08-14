use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use ctx_history_capture::{
    provider_source_for_path, register_landed_source_backed_route_with_data_root,
    SourceBackedCoordinatorError, SourceBackedProviderRegistry, SourceBackedRefreshExecutor,
    SourceBackedRefreshScope, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteSelection,
};
use ctx_history_capture_runtime::CapturePublicationDisposition;
use ctx_history_core::CaptureProvider;
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

#[test]
fn unchanged_replay_finishes_progress_and_propagates_systemic_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_opencode_database(&database);
    let index_root = temp.path().join("index");
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        provider_source_for_path(CaptureProvider::OpenCode, database),
        SourceBackedRouteSelection::ExplicitManual,
        data_root.path(),
    )
    .unwrap();
    let executor = SourceBackedRefreshExecutor::new(registry, WriterOptions::default());
    let metadata_calls = AtomicUsize::new(0);

    let mut cold = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
            |_| {
                metadata_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"logical-sqlite-replay-v1".to_vec())
            },
        )
        .unwrap();
    let cold_generation = cold.commit.generation_id.clone();
    let cold_opstamp = cold.commit.opstamp;
    assert_eq!(cold.successful_route_outcomes.len(), 1);
    assert!(cold.successful_route_outcomes[0].changed);
    assert_eq!(
        cold.take_verified_publication().unwrap().0,
        CapturePublicationDisposition::Published
    );

    let mut updates = Vec::new();
    let mut replay = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                updates.push(update);
                Ok(())
            },
            |_| {
                metadata_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"unexpected-replay-metadata".to_vec())
            },
        )
        .unwrap();
    assert_eq!(replay.commit.generation_id, cold_generation);
    assert_eq!(replay.commit.opstamp, cold_opstamp);
    assert_eq!(metadata_calls.load(Ordering::SeqCst), 1);
    assert_eq!(replay.successful_route_outcomes.len(), 1);
    assert!(!replay.successful_route_outcomes[0].changed);
    assert_eq!(
        replay.take_verified_publication().unwrap().0,
        CapturePublicationDisposition::Reused
    );
    let terminal = updates.last().expect("terminal replay progress");
    assert_eq!(terminal.progress.phase, "committed");
    assert!(terminal.current_source_progress.is_none());
    assert!(terminal.progress.current_source.is_none());
    assert!(terminal.progress.completed_records.is_none());
    assert!(terminal.progress.completed_bytes.is_none());

    let error = executor
        .refresh_scope_with_detailed_progress(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                if update.current_source_progress.is_some() {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::ResourceUnavailable,
                        "injected logical SQLite replay progress failure",
                    ));
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError {
            kind: SourceBackedRouteErrorKind::ResourceUnavailable,
            detail,
        }) if detail == "injected logical SQLite replay progress failure"
    ));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold_generation
    );
}

fn create_opencode_database(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 parent_id text,
                 directory text,
                 branch text,
                 agent text,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create unique index session_message_session_seq_idx
                 on session_message(session_id, seq);
             insert into session values (
                 'session-1', null, '/tmp/project', 'main', 'build', 1, 1
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into session_message values (
                'message-1', 'session-1', 'message', 1, 1, 1, ?1
            )",
            params![json!({
                "role": "user",
                "time": {"created": 1},
                "text": "logical SQLite facade replay"
            })
            .to_string()],
        )
        .unwrap();
}
