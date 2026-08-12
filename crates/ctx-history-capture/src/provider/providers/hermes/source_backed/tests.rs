use std::{collections::BTreeSet, fs, path::Path};

use ctx_history_core::{
    derive_event_id, CertifiedSource, EventIdentityInput, NativeItemKey, SessionRelationshipKind,
    SourceAnchor, SourceKey, TypedKey,
};
use ctx_history_index::{IndexError, VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use super::super::sqlite::{
    exact_message_query_counters, exact_message_spool_counters, hermes_message_candidate_sql,
    reset_exact_message_query_counters,
};
use super::*;
use crate::{
    automatic_source_backed_route_identity,
    provider::source_backed::{
        base_source_manifest_visits, build_automatic_source_backed_registry_from_report,
        family::document::{
            document_base_route_source_visits, reset_document_base_route_source_visits,
        },
        partial_base_route_member_visits, refresh_source_backed_generation,
        refresh_source_backed_generation_with_detailed_progress, reset_base_source_manifest_visits,
        reset_partial_base_route_member_visits, SourceBackedCoordinatorError,
        SourceBackedCurrentSourceProgressStage, SourceBackedProviderRegistry,
        SourceBackedReconciliationDemand, SourceBackedRefreshExecutor, SourceBackedRefreshReceipt,
        SourceBackedRefreshScope, SourceBackedRouteErrorKind,
    },
    provider_sources::{
        fail_next_opened_snapshot_cleanup_for_test, force_next_pinned_wal_unavailable_for_test,
        provider_source_for_path, SqliteCleanupStatus, SqliteSourceAccessError,
    },
    register_hermes_explicit_source_backed_route, DiscoveryContext, DiscoveryPlatform,
    DiscoveryPlatformDirs, DiscoveryReport,
};

const PARENT: &str = "parent-session";
const CHILD: &str = "child-session";
const PARENT_MESSAGE_ID: i64 = 10;
const CHILD_MESSAGE_ID: i64 = 20;

#[test]
fn direct_core_projection_is_complete_and_has_no_recursive_ancestry_sql() {
    let production = [
        include_str!("../source_backed.rs"),
        include_str!("replacement.rs"),
    ]
    .join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("HERMES_SOURCE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("native.complete_text"));
    assert!(production.contains("parent_session_id"));
    assert!(!production.to_ascii_lowercase().contains("with recursive"));
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn terminal_finish_and_revalidation_preserve_typed_sqlite_failures() {
    let changed = replacement::route_hermes_terminal_revalidation::<()>(Err(
        SqliteSourceAccessError::SourceChanged,
    ))
    .unwrap_err();
    assert_eq!(changed.kind, SourceBackedRouteErrorKind::SourceChanged);

    let cleanup = SqliteSourceAccessError::ScratchIoUnavailable {
        operation: "cleaning the Hermes terminal regression snapshot",
        path: "hermes-terminal.sqlite".into(),
        source: std::io::Error::from(std::io::ErrorKind::StorageFull),
    }
    .with_cleanup_status(SqliteCleanupStatus::Failed);
    let cleanup = replacement::route_hermes_terminal_revalidation::<()>(Err(cleanup)).unwrap_err();
    assert_eq!(
        cleanup.kind,
        SourceBackedRouteErrorKind::ResourceUnavailable
    );
    assert!(cleanup.detail.contains("cleanup_status=failed"));
}

fn create_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 ended_at real,
                 message_count integer default 0,
                 cwd text,
                 git_branch text,
                 git_repo_root text
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null,
                 active integer not null default 1,
                 compacted integer not null default 0
             );
             insert into sessions
                 (id, source, parent_session_id, started_at, message_count, cwd, git_branch, git_repo_root)
                 values
                 ('parent-session', 'acp', null, 1782259200.0, 1, '/repo/parent', 'main', '/repo'),
                 ('child-session', 'acp', 'parent-session', 1782259201.0, 1, '/repo/child', 'feature', '/repo');
             insert into messages (id, session_id, role, content, timestamp) values
                 (10, 'parent-session', 'assistant', 'parent stable needle', 1782259202.0),
                 (20, 'child-session', 'assistant', 'child stable needle', 1782259203.0);",
        )
        .unwrap();
}

fn create_many_session_fixture(path: &Path, sessions: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null,
                 message_count integer default 0
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert_session = transaction
            .prepare(
                "insert into sessions (id, source, started_at, message_count)
                 values (?1, 'acp', ?2, 1)",
            )
            .unwrap();
        let mut insert_message = transaction
            .prepare(
                "insert into messages (id, session_id, role, content, timestamp)
                 values (?1, ?2, 'assistant', ?3, ?4)",
            )
            .unwrap();
        for index in 0..sessions {
            let session = format!("session-{index:04}");
            insert_session
                .execute((&session, 1_782_259_200_f64 + index as f64))
                .unwrap();
            insert_message
                .execute((
                    i64::try_from(index + 1).unwrap(),
                    &session,
                    format!("body {index:04}"),
                    1_782_260_000_f64 + index as f64,
                ))
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn fixture_registry(data_root: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, database.to_path_buf()),
        data_root,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap();
    registry
}

#[test]
fn automatic_multiplex_profiles_register_and_refresh_every_discovered_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let profiles = temp.path().join("profiles");
    let alpha = profiles.join("alpha/state.db");
    let beta = profiles.join("beta-2/state.db");
    let default = temp.path().join("default/state.db");
    create_fixture(&default);
    create_fixture(&alpha);
    create_fixture(&beta);
    let sources = [&default, &alpha, &beta]
        .into_iter()
        .map(|path| provider_source_for_path(CaptureProvider::Hermes, path.to_path_buf()))
        .collect::<Vec<_>>();
    let route_ids = sources
        .iter()
        .map(automatic_source_backed_route_identity)
        .collect::<Result<BTreeSet<_>, _>>()
        .unwrap();
    assert_eq!(route_ids.len(), 3);

    let discovery = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let build = build_automatic_source_backed_registry_from_report(
        &discovery,
        &data_root,
        DiscoveryReport {
            sources,
            issues: Vec::new(),
        },
    );
    assert!(build.issues.is_empty());
    assert_eq!(build.executable_route_count(), 3);
    let refreshed =
        refresh_source_backed_generation(&index_root, &build.registry, fixture_writer_options())
            .unwrap();
    assert_eq!(refreshed.sources.len(), 6);
    assert_eq!(refreshed.route_controls.len(), 3);
    assert_eq!(refreshed.successful_route_ids.len(), 3);

    let invalid = profiles.join("Bad.Name/state.db");
    assert!(matches!(
        HermesSourceCandidate::automatic(
            &data_root,
            provider_source_for_path(CaptureProvider::Hermes, invalid)
        ),
        Err(HermesSourceBackedError::InvalidProfilePath(_))
    ));
}

#[test]
fn renamed_automatic_profile_rejects_the_old_profile_control_before_incremental_reuse() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let alpha = temp.path().join("profiles/alpha/state.db");
    let beta = temp.path().join("profiles/beta/state.db");
    create_fixture(&alpha);
    let alpha_candidate = HermesSourceCandidate::automatic(
        &data_root,
        provider_source_for_path(CaptureProvider::Hermes, alpha.clone()),
    )
    .unwrap();
    let registry = build_automatic_source_backed_registry_from_report(
        &DiscoveryContext::new(
            temp.path(),
            temp.path(),
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        ),
        &data_root,
        DiscoveryReport {
            sources: vec![provider_source_for_path(CaptureProvider::Hermes, alpha)],
            issues: Vec::new(),
        },
    )
    .registry;
    let index_root = temp.path().join("index");
    let cold =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    let prior: HermesRefreshReceipt =
        serde_json::from_slice(cold.route_controls.values().next().expect("Hermes control"))
            .unwrap();

    fs::rename(
        temp.path().join("profiles/alpha"),
        temp.path().join("profiles/beta"),
    )
    .unwrap();
    let beta_candidate = HermesSourceCandidate::automatic(
        &data_root,
        provider_source_for_path(CaptureProvider::Hermes, beta.clone()),
    )
    .unwrap();
    assert_ne!(
        alpha_candidate.source.identity(),
        beta_candidate.source.identity()
    );
    assert_eq!(
        hermes_route_control_exact_due_for_profile(
            cold.route_controls.values().next().expect("Hermes control"),
            beta_candidate.source.exact_descriptor_digest(),
            i64::MIN,
        ),
        None
    );
    assert!(hermes_incremental_requires_exhaustive(
        &Connection::open(beta).unwrap(),
        &prior,
        beta_candidate.source.exact_descriptor_digest(),
        [0; 32],
    )
    .unwrap());
}

fn fixture_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn candidate(data_root: &Path, database: &Path) -> HermesSourceCandidate {
    hermes_source_backed_explicit(
        data_root,
        database,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap()
}

fn session_source(candidate: &HermesSourceCandidate, session: &str) -> SourceKey {
    hermes_session_source_key(&candidate.source, session).unwrap()
}

fn event_id(
    source: &SourceKey,
    session: &str,
    message_id: i64,
) -> ctx_history_core::StableEntityId {
    let session_id = hermes_session_id(source, session).unwrap();
    let native_item_key = NativeItemKey::composite(
        HERMES_MESSAGE_NAMESPACE,
        vec![TypedKey::utf8(session).unwrap(), TypedKey::I64(message_id)],
    )
    .unwrap();
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: HERMES_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap()
}

fn source_certificate<'a>(
    receipt: &'a SourceBackedRefreshReceipt,
    source: &SourceKey,
) -> &'a CertifiedSource {
    receipt
        .sources
        .iter()
        .find(|certificate| {
            certificate
                .observation()
                .source()
                .exact_descriptor_eq(source)
        })
        .expect("Hermes session certificate")
}

fn indexed_record(
    index_root: &Path,
    source: &SourceKey,
    session: &str,
    message_id: i64,
) -> CoreRecord {
    VerifiedIndex::open(index_root)
        .unwrap()
        .core_record_by_id(event_id(source, session, message_id).as_uuid())
        .unwrap()
        .expect("Hermes indexed record")
}

fn assert_search_contains(index_root: &Path, needle: &str, expected: &CoreRecord) {
    let candidates = VerifiedIndex::open(index_root)
        .unwrap()
        .search_event_candidates(needle, 8)
        .unwrap();
    assert!(candidates
        .iter()
        .any(|candidate| candidate.event.event_id == expected.event_id));
}

fn cold_fixture(
    data_root: &Path,
    index_root: &Path,
    database: &Path,
) -> (
    SourceBackedProviderRegistry,
    HermesSourceCandidate,
    SourceBackedRefreshReceipt,
) {
    create_fixture(database);
    let registry = fixture_registry(data_root, database);
    let candidate = candidate(data_root, database);
    reset_logical_row_traversals();
    reset_exact_message_query_counters();
    let cold =
        refresh_source_backed_generation(index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(logical_row_traversals(), 2);
    assert_eq!(inventory_observation_rows(), 4);
    (registry, candidate, cold)
}

fn incremental_refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    base: &SourceBackedRefreshReceipt,
) -> SourceBackedRefreshReceipt {
    SourceBackedRefreshExecutor::new(registry.clone(), fixture_writer_options())
        .with_base_route_controls(base.route_controls.clone())
        .refresh_scope_with_detailed_progress_and_reconciliation(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            |_| Ok(()),
        )
        .unwrap()
}

fn exhaustive_refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    base: &SourceBackedRefreshReceipt,
) -> SourceBackedRefreshReceipt {
    SourceBackedRefreshExecutor::new(registry.clone(), fixture_writer_options())
        .with_base_route_controls(base.route_controls.clone())
        .refresh_scope_with_detailed_progress_and_reconciliation(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
            |_| Ok(()),
        )
        .unwrap()
}

fn rewritten_route_controls(
    receipt: &SourceBackedRefreshReceipt,
    rewrite: impl FnOnce(&mut HermesRefreshReceipt),
) -> std::collections::BTreeMap<ctx_history_index::SourceRouteIdentity, Vec<u8>> {
    let mut controls = receipt.route_controls.clone();
    assert_eq!(controls.len(), 1);
    let control = controls.values_mut().next().unwrap();
    let mut parsed: HermesRefreshReceipt = serde_json::from_slice(control).unwrap();
    rewrite(&mut parsed);
    *control = serde_json::to_vec(&parsed).unwrap();
    controls
}

#[test]
fn production_incremental_noop_and_append_are_delta_proportional() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = hermes_session_source_key(&candidate.source, PARENT).unwrap();
    let child_source = hermes_session_source_key(&candidate.source, CHILD).unwrap();
    let child_certificate = source_certificate(&cold, &child_source).clone();

    reset_logical_row_traversals();
    reset_document_base_route_source_visits();
    reset_base_source_manifest_visits();
    reset_partial_base_route_member_visits();
    let noop = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(inventory_observation_rows(), 0);
    assert_eq!(logical_row_traversals(), 0);
    assert_eq!(document_base_route_source_visits(), 0);
    assert_eq!(base_source_manifest_visits(), 0);
    assert_eq!(partial_base_route_member_visits(), 0);
    assert!(session_scan_receipts().is_empty());

    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp) values
             (30, 'parent-session', 'assistant', 'incremental append needle', 1782259204.0)",
            [],
        )
        .unwrap();
    reset_logical_row_traversals();
    reset_document_base_route_source_visits();
    reset_base_source_manifest_visits();
    reset_partial_base_route_member_visits();
    let appended = incremental_refresh(&index_root, &registry, &noop);
    assert_eq!(appended.sources.len(), 2);
    assert_eq!(
        source_certificate(&appended, &child_source),
        &child_certificate
    );
    assert_eq!(inventory_observation_rows(), 1);
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(document_base_route_source_visits(), 1);
    assert_eq!(base_source_manifest_visits(), 0);
    assert_eq!(partial_base_route_member_visits(), 0);
    assert_eq!(
        session_scan_receipts().keys().cloned().collect::<Vec<_>>(),
        vec![PARENT.to_owned()]
    );
    let appended_record = indexed_record(&index_root, &parent_source, PARENT, 30);
    assert_search_contains(&index_root, "incremental append needle", &appended_record);
}

#[test]
fn production_incremental_base_route_work_stays_touch_bounded_with_large_history() {
    const SESSIONS: usize = 129;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database, SESSIONS);
    let registry = fixture_registry(&data_root, &database);
    let cold =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(cold.sources.len(), SESSIONS);

    reset_logical_row_traversals();
    reset_document_base_route_source_visits();
    reset_base_source_manifest_visits();
    reset_partial_base_route_member_visits();
    let noop = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(inventory_observation_rows(), 0);
    assert_eq!(logical_row_traversals(), 0);
    assert_eq!(document_base_route_source_visits(), 0);
    assert_eq!(base_source_manifest_visits(), 0);
    assert_eq!(partial_base_route_member_visits(), 0);

    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (?1, 'session-0128', 'assistant', 'large delta needle', 1782269999.0)",
            [i64::try_from(SESSIONS + 1).unwrap()],
        )
        .unwrap();
    reset_logical_row_traversals();
    reset_document_base_route_source_visits();
    reset_base_source_manifest_visits();
    reset_partial_base_route_member_visits();
    let appended = incremental_refresh(&index_root, &registry, &noop);
    assert_eq!(appended.sources.len(), SESSIONS);
    assert_eq!(inventory_observation_rows(), 1);
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(document_base_route_source_visits(), 1);
    assert_eq!(base_source_manifest_visits(), 0);
    assert_eq!(partial_base_route_member_visits(), 0);
}

#[test]
fn production_incremental_new_and_empty_sessions_read_only_the_delta() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent = source_certificate(&cold, &session_source(&candidate, PARENT)).clone();
    let child = source_certificate(&cold, &session_source(&candidate, CHILD)).clone();

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "insert into sessions (id, source, started_at, message_count)
                 values ('new-empty', 'acp', 1782259210.0, 0),
                        ('new-full', 'acp', 1782259211.0, 1);
             insert into messages (id, session_id, role, content, timestamp)
                 values (30, 'new-full', 'assistant', 'new session delta needle', 1782259212.0);",
        )
        .unwrap();
    reset_logical_row_traversals();
    reset_base_source_manifest_visits();
    reset_partial_base_route_member_visits();
    let refreshed = incremental_refresh(&index_root, &registry, &cold);

    assert_eq!(refreshed.sources.len(), 4);
    assert_eq!(inventory_observation_rows(), 3);
    assert_eq!(logical_row_traversals(), 2);
    assert_eq!(base_source_manifest_visits(), 0);
    assert_eq!(partial_base_route_member_visits(), 0);
    assert_eq!(
        session_scan_receipts().keys().cloned().collect::<Vec<_>>(),
        vec!["new-empty".to_owned(), "new-full".to_owned()]
    );
    assert_eq!(
        source_certificate(&refreshed, &session_source(&candidate, PARENT)),
        &parent
    );
    assert_eq!(
        source_certificate(&refreshed, &session_source(&candidate, CHILD)),
        &child
    );
    let new_source = session_source(&candidate, "new-full");
    let new_record = indexed_record(&index_root, &new_source, "new-full", 30);
    assert_search_contains(&index_root, "new session delta needle", &new_record);
    assert_eq!(
        source_certificate(&refreshed, &session_source(&candidate, "new-empty"))
            .counts()
            .indexed_documents,
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn production_incremental_reads_an_active_wal_or_defers_safely_as_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let writer = Connection::open(&database).unwrap();
    assert_eq!(
        writer
            .pragma_update_and_check(None, "journal_mode", "wal", |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .to_ascii_lowercase(),
        "wal"
    );
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (30, 'parent-session', 'assistant', 'active wal delta needle', 1782259216.0)",
            [],
        )
        .unwrap();
    assert!(database.with_extension("db-wal").exists());

    reset_logical_row_traversals();
    let refreshed = incremental_refresh(&index_root, &registry, &cold);
    if unsafe { libc::geteuid() } == 0 {
        assert_eq!(refreshed.commit.generation_id, cold.commit.generation_id);
        assert_eq!(refreshed.route_controls, cold.route_controls);
        assert_eq!(inventory_observation_rows(), 0);
        return;
    }
    assert_eq!(inventory_observation_rows(), 1);
    assert_eq!(logical_row_traversals(), 1);
    let record = indexed_record(&index_root, &session_source(&candidate, PARENT), PARENT, 30);
    assert_search_contains(&index_root, "active wal delta needle", &record);
    drop(writer);
}

#[test]
fn unavailable_incremental_fast_path_defers_without_copy_or_partial_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, _, cold) = cold_fixture(&data_root, &index_root, &database);
    assert!(!database.with_extension("db-wal").exists());

    force_next_pinned_wal_unavailable_for_test();
    let deferred = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(deferred.commit.generation_id, cold.commit.generation_id);
    assert_eq!(deferred.sources, cold.sources);
    assert_eq!(deferred.route_controls, cold.route_controls);
    assert!(deferred.failed_routes.is_empty());
    assert!(fs::read_dir(data_root.join("tmp/provider-sqlite"))
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true));
}

#[test]
fn incremental_rewrite_active_flip_and_deletion_stay_stale_until_exhaustive() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let parent_before = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'same row rewritten', active = 0 where id = ?1",
            [PARENT_MESSAGE_ID],
        )
        .unwrap();
    reset_logical_row_traversals();
    let stale_rewrite = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(inventory_observation_rows(), 0);
    assert_eq!(logical_row_traversals(), 0);
    assert_eq!(
        indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID),
        parent_before
    );

    let exact_rewrite = exhaustive_refresh(&index_root, &registry, &stale_rewrite);
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(parent_before.event_id.as_uuid())
        .unwrap()
        .is_none());
    assert!(exact_rewrite.sources.iter().any(|source| source
        .observation()
        .source()
        .exact_descriptor_eq(&parent_source)));

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "delete from messages where id = 10;
             delete from sessions where id = 'parent-session';",
        )
        .unwrap();
    reset_logical_row_traversals();
    let stale_delete = incremental_refresh(&index_root, &registry, &exact_rewrite);
    assert_eq!(inventory_observation_rows(), 0);
    assert!(stale_delete.sources.iter().any(|source| source
        .observation()
        .source()
        .exact_descriptor_eq(&parent_source)));

    let exact_delete = exhaustive_refresh(&index_root, &registry, &stale_delete);
    assert!(!exact_delete.sources.iter().any(|source| source
        .observation()
        .source()
        .exact_descriptor_eq(&parent_source)));
    assert_eq!(exact_delete.sources.len(), 1);
}

#[test]
fn cursor_regression_and_database_replacement_force_exhaustive_reconciliation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);

    let regressed_controls = rewritten_route_controls(&cold, |control| {
        control.session_rowid = i64::MAX;
        control.message_rowid = i64::MAX;
    });
    reset_logical_row_traversals();
    let regression = SourceBackedRefreshExecutor::new(registry.clone(), fixture_writer_options())
        .with_base_route_controls(regressed_controls)
        .refresh_scope_with_detailed_progress_and_reconciliation(
            &index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(inventory_observation_rows(), 4);

    fs::rename(&database, database.with_extension("old")).unwrap();
    create_fixture(&database);
    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'replacement database needle' where id = 10",
            [],
        )
        .unwrap();
    reset_logical_row_traversals();
    let _replacement = incremental_refresh(&index_root, &registry, &regression);
    assert_eq!(inventory_observation_rows(), 4);
    let parent = indexed_record(
        &index_root,
        &session_source(&candidate, PARENT),
        PARENT,
        PARENT_MESSAGE_ID,
    );
    assert_search_contains(&index_root, "replacement database needle", &parent);
}

#[test]
fn incremental_cursor_advances_only_with_successful_core_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    Connection::open(&database)
        .unwrap()
        .execute(
            "insert into messages (id, session_id, role, content, timestamp)
             values (30, 'parent-session', 'assistant', 'atomic cursor needle', 1782259215.0)",
            [],
        )
        .unwrap();

    let failed = SourceBackedRefreshExecutor::new(registry.clone(), fixture_writer_options())
        .with_base_route_controls(cold.route_controls.clone())
        .refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
            &index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            |_| Ok(()),
            |_| {
                Err(IndexError::PublicationMetadata(
                    "injected Hermes publication failure".into(),
                ))
            },
        );
    assert!(failed.is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );

    reset_logical_row_traversals();
    let retry = incremental_refresh(&index_root, &registry, &cold);
    assert_eq!(inventory_observation_rows(), 1);
    assert_eq!(logical_row_traversals(), 1);
    let record = indexed_record(&index_root, &session_source(&candidate, PARENT), PARENT, 30);
    assert_search_contains(&index_root, "atomic cursor needle", &record);
    assert_ne!(retry.route_controls, cold.route_controls);
}

#[test]
fn failed_exhaustive_publication_retains_due_control_and_retry_converges() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let overdue_controls = rewritten_route_controls(&cold, |control| {
        control.last_successful_exhaustive_at_ms = 0;
        control.exact_due_at_ms = 0;
    });
    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'exhaustive retry needle' where id = 10",
            [],
        )
        .unwrap();

    let failed = SourceBackedRefreshExecutor::new(registry.clone(), fixture_writer_options())
        .with_base_route_controls(overdue_controls.clone())
        .refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
            &index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
            |_| Ok(()),
            |_| {
                Err(IndexError::PublicationMetadata(
                    "injected exhaustive failure".into(),
                ))
            },
        );
    assert!(failed.is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
    let due = overdue_controls.values().next().unwrap();
    assert_eq!(hermes_route_control_exact_due(due, 1), Some(true));

    let retry = SourceBackedRefreshExecutor::new(registry, fixture_writer_options())
        .with_base_route_controls(overdue_controls)
        .refresh_scope_with_detailed_progress_and_reconciliation(
            &index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
            |_| Ok(()),
        )
        .unwrap();
    let record = indexed_record(
        &index_root,
        &session_source(&candidate, PARENT),
        PARENT,
        PARENT_MESSAGE_ID,
    );
    assert_search_contains(&index_root, "exhaustive retry needle", &record);
    assert_eq!(
        hermes_route_control_exact_due(retry.route_controls.values().next().unwrap(), 1),
        Some(false)
    );
}

#[test]
fn exhaustive_profile_removal_cannot_claim_sibling_profile_sources_or_control() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let first_database = temp.path().join("first/state.db");
    let second_database = temp.path().join("second/state.db");
    create_fixture(&first_database);
    create_fixture(&second_database);
    Connection::open(&second_database)
        .unwrap()
        .execute(
            "update messages set content = 'sibling profile needle' where id = 10",
            [],
        )
        .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, first_database.clone()),
        &data_root,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, second_database.clone()),
        &data_root,
        SourceAnchor::CatalogLineage([0x49; 32]),
    )
    .unwrap();
    let first = candidate(&data_root, &first_database);
    let second = hermes_source_backed_explicit(
        &data_root,
        &second_database,
        SourceAnchor::CatalogLineage([0x49; 32]),
    )
    .unwrap();
    let cold =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 4);
    assert_eq!(cold.route_controls.len(), 2);
    let sibling_source = session_source(&second, PARENT);
    let sibling_certificate = source_certificate(&cold, &sibling_source).clone();

    Connection::open(&first_database)
        .unwrap()
        .execute_batch(
            "delete from messages where session_id = 'parent-session';
             delete from sessions where id = 'parent-session';",
        )
        .unwrap();
    let refreshed = exhaustive_refresh(&index_root, &registry, &cold);
    assert_eq!(refreshed.sources.len(), 3);
    assert_eq!(refreshed.route_controls.len(), 2);
    assert_eq!(
        source_certificate(&refreshed, &sibling_source),
        &sibling_certificate
    );
    assert!(!refreshed.sources.iter().any(|source| {
        source
            .observation()
            .source()
            .exact_descriptor_eq(&session_source(&first, PARENT))
    }));
    let sibling = indexed_record(&index_root, &sibling_source, PARENT, PARENT_MESSAGE_ID);
    assert_search_contains(&index_root, "sibling profile needle", &sibling);
}

fn assert_unchanged_session(
    index_root: &Path,
    receipt: &SourceBackedRefreshReceipt,
    source: &SourceKey,
    baseline_certificate: &CertifiedSource,
    baseline_record: &CoreRecord,
    needle: &str,
    session: &str,
    message_id: i64,
) {
    assert_eq!(source_certificate(receipt, source), baseline_certificate);
    let current = indexed_record(index_root, source, session, message_id);
    assert_eq!(&current, baseline_record);
    assert_eq!(
        serde_json::to_vec(&current).unwrap(),
        serde_json::to_vec(baseline_record).unwrap()
    );
    assert_search_contains(index_root, needle, &current);
}

#[test]
fn session_source_keys_are_profile_scoped_and_stable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = hermes_source_backed_explicit(
        temp.path().join("data"),
        temp.path().join("one/state.db"),
        SourceAnchor::CatalogLineage([1; 32]),
    )
    .unwrap();
    let same = hermes_session_source_key(&first.source, CHILD).unwrap();
    let replay = hermes_session_source_key(&first.source, CHILD).unwrap();
    let sibling = hermes_session_source_key(&first.source, PARENT).unwrap();
    let second = hermes_source_backed_explicit(
        temp.path().join("data"),
        temp.path().join("two/state.db"),
        SourceAnchor::CatalogLineage([2; 32]),
    )
    .unwrap();
    let other_profile = hermes_session_source_key(&second.source, CHILD).unwrap();
    assert!(same.exact_descriptor_eq(&replay));
    assert_ne!(same.identity(), sibling.identity());
    assert_ne!(same.identity(), other_profile.identity());
    assert_eq!(same.schema_variant(), HERMES_SESSION_SOURCE_SCHEMA_VARIANT);
}

#[test]
fn parent_append_and_rewrite_leave_child_byte_identical() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let child_source = session_source(&candidate, CHILD);
    let child_certificate = source_certificate(&cold, &child_source).clone();
    let child_record = indexed_record(&index_root, &child_source, CHILD, CHILD_MESSAGE_ID);
    let parent_record = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);
    assert_eq!(
        child_record.session_relationship,
        SessionRelationshipKind::Delegated
    );
    assert_eq!(
        child_record.parent_session_id,
        Some(parent_record.session_id)
    );
    assert_eq!(child_record.root_session_id, parent_record.session_id);
    assert_search_contains(&index_root, "child stable needle", &child_record);

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "insert into messages (id, session_id, role, content, timestamp)
                 values (11, 'parent-session', 'assistant', 'parent appended', 1782259204.0);
             update sessions set message_count = 2, ended_at = 1782259204.0
                 where id = 'parent-session';",
        )
        .unwrap();
    reset_logical_row_traversals();
    let appended =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![PARENT]
    );
    assert_ne!(
        source_certificate(&appended, &parent_source),
        source_certificate(&cold, &parent_source)
    );
    assert_unchanged_session(
        &index_root,
        &appended,
        &child_source,
        &child_certificate,
        &child_record,
        "child stable needle",
        CHILD,
        CHILD_MESSAGE_ID,
    );

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "update messages set content = 'parent rewritten', timestamp = 1782259210.0
                 where id = 10;
             update sessions set ended_at = 1782259210.0 where id = 'parent-session';",
        )
        .unwrap();
    reset_logical_row_traversals();
    let rewritten =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![PARENT]
    );
    assert_unchanged_session(
        &index_root,
        &rewritten,
        &child_source,
        &child_certificate,
        &child_record,
        "child stable needle",
        CHILD,
        CHILD_MESSAGE_ID,
    );
}

#[test]
fn same_key_same_timestamp_body_rewrite_replaces_only_parent_source() {
    const ORIGINAL: &str = "parent stable needle";
    const REWRITTEN: &str = "parent edited needle";
    assert_eq!(ORIGINAL.len(), REWRITTEN.len());

    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let child_source = session_source(&candidate, CHILD);
    let parent_certificate = source_certificate(&cold, &parent_source).clone();
    let parent_record = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);
    let child_certificate = source_certificate(&cold, &child_source).clone();
    let child_record = indexed_record(&index_root, &child_source, CHILD, CHILD_MESSAGE_ID);

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = ?1 where id = ?2",
            (REWRITTEN, PARENT_MESSAGE_ID),
        )
        .unwrap();
    reset_logical_row_traversals();
    let rewritten =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();

    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![PARENT]
    );
    assert_ne!(
        source_certificate(&rewritten, &parent_source),
        &parent_certificate
    );
    let rewritten_parent = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);
    assert_eq!(rewritten_parent.event_id, parent_record.event_id);
    assert_ne!(rewritten_parent, parent_record);
    assert_search_contains(&index_root, "edited", &rewritten_parent);
    assert!(!VerifiedIndex::open(&index_root)
        .unwrap()
        .search_event_candidates("stable", 8)
        .unwrap()
        .iter()
        .any(|candidate| candidate.event.event_id == rewritten_parent.event_id));
    assert_unchanged_session(
        &index_root,
        &rewritten,
        &child_source,
        &child_certificate,
        &child_record,
        "child stable needle",
        CHILD,
        CHILD_MESSAGE_ID,
    );
}

#[test]
fn parent_delete_and_reappear_remove_and_scan_only_parent_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let child_source = session_source(&candidate, CHILD);
    let child_certificate = source_certificate(&cold, &child_source).clone();
    let child_record = indexed_record(&index_root, &child_source, CHILD, CHILD_MESSAGE_ID);
    let parent_event_id = event_id(&parent_source, PARENT, PARENT_MESSAGE_ID);

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "delete from messages where session_id = 'parent-session';
             delete from sessions where id = 'parent-session';",
        )
        .unwrap();
    reset_logical_row_traversals();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert!(session_scan_receipts().is_empty());
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    assert!(deleted.removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(&parent_source));
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(parent_event_id.as_uuid())
        .unwrap()
        .is_none());
    assert_unchanged_session(
        &index_root,
        &deleted,
        &child_source,
        &child_certificate,
        &child_record,
        "child stable needle",
        CHILD,
        CHILD_MESSAGE_ID,
    );

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "insert into sessions
                 (rowid, id, source, parent_session_id, started_at, message_count, cwd, git_branch, git_repo_root)
                 values (1, 'parent-session', 'acp', null, 1782259200.0, 1, '/repo/parent', 'main', '/repo');
             insert into messages (id, session_id, role, content, timestamp)
                 values (10, 'parent-session', 'assistant', 'parent reappeared', 1782259260.0);",
        )
        .unwrap();
    reset_logical_row_traversals();
    let reappeared =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![PARENT]
    );
    assert_eq!(reappeared.sources.len(), 2);
    assert_eq!(
        indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID).event_id,
        parent_event_id
    );
    assert_unchanged_session(
        &index_root,
        &reappeared,
        &child_source,
        &child_certificate,
        &child_record,
        "child stable needle",
        CHILD,
        CHILD_MESSAGE_ID,
    );
}

#[test]
fn child_only_and_simultaneous_mutations_scan_exact_changed_sources() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let child_source = session_source(&candidate, CHILD);
    let parent_certificate = source_certificate(&cold, &parent_source).clone();
    let parent_record = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'child changed', timestamp = 1782259220.0 where id = 20",
            [],
        )
        .unwrap();
    reset_logical_row_traversals();
    let child_only =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![CHILD]
    );
    assert_unchanged_session(
        &index_root,
        &child_only,
        &parent_source,
        &parent_certificate,
        &parent_record,
        "parent stable needle",
        PARENT,
        PARENT_MESSAGE_ID,
    );

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "update messages set content = 'parent simultaneous', timestamp = 1782259230.0 where id = 10;
             update messages set content = 'child simultaneous', timestamp = 1782259231.0 where id = 20;",
        )
        .unwrap();
    reset_logical_row_traversals();
    let simultaneous =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([PARENT.to_owned(), CHILD.to_owned()])
    );
    assert_ne!(
        source_certificate(&simultaneous, &parent_source),
        &parent_certificate
    );
    assert_ne!(
        source_certificate(&simultaneous, &child_source),
        source_certificate(&child_only, &child_source)
    );
}

#[test]
fn delete_and_reappear_reuses_only_the_deleted_session_lineage() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    let (registry, candidate, cold) = cold_fixture(&data_root, &index_root, &database);
    let parent_source = session_source(&candidate, PARENT);
    let child_source = session_source(&candidate, CHILD);
    let parent_certificate = source_certificate(&cold, &parent_source).clone();
    let parent_record = indexed_record(&index_root, &parent_source, PARENT, PARENT_MESSAGE_ID);
    let child_event_id = event_id(&child_source, CHILD, CHILD_MESSAGE_ID);

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "delete from messages where session_id = 'child-session';
             delete from sessions where id = 'child-session';",
        )
        .unwrap();
    reset_logical_row_traversals();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert!(session_scan_receipts().is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(child_event_id.as_uuid())
        .unwrap()
        .is_none());
    assert_unchanged_session(
        &index_root,
        &deleted,
        &parent_source,
        &parent_certificate,
        &parent_record,
        "parent stable needle",
        PARENT,
        PARENT_MESSAGE_ID,
    );

    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "insert into sessions
                 (id, source, parent_session_id, started_at, message_count, cwd, git_branch, git_repo_root)
                 values ('child-session', 'acp', 'parent-session', 1782259240.0, 1, '/repo/child', 'feature', '/repo');
             insert into messages (id, session_id, role, content, timestamp)
                 values (20, 'child-session', 'assistant', 'child reappeared', 1782259241.0);",
        )
        .unwrap();
    reset_logical_row_traversals();
    let reappeared =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec![CHILD]
    );
    assert_eq!(reappeared.sources.len(), 2);
    let reappeared_child = indexed_record(&index_root, &child_source, CHILD, CHILD_MESSAGE_ID);
    assert_eq!(reappeared_child.event_id, child_event_id);
    assert_unchanged_session(
        &index_root,
        &reappeared,
        &parent_source,
        &parent_certificate,
        &parent_record,
        "parent stable needle",
        PARENT,
        PARENT_MESSAGE_ID,
    );
}

#[test]
fn noop_and_one_changed_session_have_linear_inventory_and_changed_body_work() {
    const SESSIONS: usize = 129;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database, SESSIONS);
    let registry = fixture_registry(&data_root, &database);

    reset_logical_row_traversals();
    reset_exact_message_query_counters();
    let cold =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(cold.sources.len(), SESSIONS);
    assert_eq!(logical_row_traversals(), SESSIONS as u64);
    assert_eq!(inventory_observation_rows(), (SESSIONS * 2) as u64);
    assert_eq!(exact_message_query_counters(), (1, 0));
    assert_eq!(
        exact_message_spool_counters(),
        (1, 0, 1, SESSIONS as u64, SESSIONS as u64)
    );

    reset_logical_row_traversals();
    reset_exact_message_query_counters();
    let noop =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(inventory_observation_rows(), (SESSIONS * 2) as u64);
    assert_eq!(logical_row_traversals(), 0);
    assert_eq!(exact_message_query_counters(), (1, 0));
    assert_eq!(exact_message_spool_counters(), (1, 0, 0, 0, 0));
    assert!(session_scan_receipts().is_empty());

    Connection::open(&database)
        .unwrap()
        .execute(
            "update messages set content = 'one changed body', timestamp = 1782269999.0 where id = 65",
            [],
        )
        .unwrap();
    reset_logical_row_traversals();
    reset_exact_message_query_counters();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(inventory_observation_rows(), (SESSIONS * 2) as u64);
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(exact_message_query_counters(), (1, 0));
    assert_eq!(exact_message_spool_counters(), (1, 0, 1, 1, 1));
    assert_eq!(
        session_scan_receipts().keys().collect::<Vec<_>>(),
        vec!["session-0064"]
    );
    let (parsed_rows, body_queries) = session_scan_receipts()["session-0064"];
    assert_eq!(parsed_rows, 2);
    assert!(body_queries > 0);
}

#[test]
fn exhaustive_source_backed_projection_includes_minimum_message_rowid() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "drop table messages;
             create table messages (
                 id integer not null,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null,
                 active integer not null default 1,
                 compacted integer not null default 0
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into messages (rowid, id, session_id, role, content, timestamp)
             values (?1, 30, 'parent-session', 'assistant', 'minimum rowid needle', 1782259204.0)",
            [i64::MIN],
        )
        .unwrap();
    drop(connection);

    let registry = fixture_registry(&data_root, &database);
    let candidate = candidate(&data_root, &database);
    reset_exact_message_query_counters();
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();

    assert_eq!(receipt.sources.len(), 2);
    assert_eq!(exact_message_query_counters(), (1, 0));
    let parent_source = hermes_session_source_key(&candidate.source, PARENT).unwrap();
    let record = indexed_record(&index_root, &parent_source, PARENT, 30);
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("minimum rowid needle")
    );
}

#[test]
fn exact_message_traversal_uses_the_rowid_plan_without_a_session_index_or_sort() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/state.db");
    create_many_session_fixture(&database, 8);
    let conn = Connection::open(&database).unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let sql = hermes_message_candidate_sql(
        &schema.messages().retained_length_expr(),
        &schema.messages().storage_class_error_expr(),
        schema.message_visibility(),
        true,
        false,
    );
    let mut statement = conn.prepare(&format!("explain query plan {sql}")).unwrap();
    let details = statement
        .query_map([i64::MIN], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(details.iter().any(|detail| {
        detail.contains("SEARCH m USING INTEGER PRIMARY KEY") && detail.contains("rowid>?")
    }));
    assert!(details
        .iter()
        .all(|detail| !detail.contains("USE TEMP B-TREE")));
}

fn projected_bodies(
    candidate: &HermesSourceCandidate,
    snapshot: &SqliteSourceReadSnapshot,
    session: &str,
) -> Vec<String> {
    let mut inventory = observe_hermes_session_inventory(
        candidate,
        snapshot.connection().unwrap(),
        &mut |_| Ok(()),
    )
    .unwrap();
    let leaf = inventory
        .leaves
        .iter()
        .find(|leaf| leaf.provider_leaf.provider_session_id == session)
        .unwrap()
        .provider_leaf
        .clone();
    let mut bodies = Vec::new();
    project_hermes_session_snapshot(
        candidate,
        &leaf,
        &inventory.schema,
        snapshot.connection().unwrap(),
        inventory.message_spool.as_mut().unwrap(),
        &mut |page| {
            bodies.extend(page.records.into_iter().filter_map(|record| match record {
                HermesSourceBackedRecord::Event(event) => event.content.normalized_body,
                HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => {
                    None
                }
            }));
            Ok(())
        },
    )
    .unwrap();
    bodies
}

#[test]
fn concurrent_commit_cannot_mix_inventory_and_body_observations() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let candidate = candidate(&data_root, &database);
    let (_authority, baseline) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    baseline.finish().unwrap();
    let bytes_before = provider_family_bytes(&database);

    let (_authority, snapshot) =
        open_root_authorized_snapshot_with_hook(&data_root, &database, || {
            writer
                .execute_batch(
                    "insert into messages (id, session_id, role, content, timestamp)
                         values (11, 'parent-session', 'assistant', 'racing append', 1782259250.0);
                     update sessions set message_count = 2 where id = 'parent-session';",
                )
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        projected_bodies(&candidate, &snapshot, PARENT),
        vec!["parent stable needle"]
    );
    snapshot.finish().unwrap();

    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    assert_eq!(
        projected_bodies(&candidate, &snapshot, PARENT),
        vec!["parent stable needle", "racing append"]
    );
    snapshot.finish().unwrap();
    assert!(provider_directory_names(&database)
        .iter()
        .all(|name| matches!(name.as_str(), "state.db" | "state.db-shm" | "state.db-wal")));
    assert_ne!(provider_family_bytes(&database), bytes_before);
}

#[test]
fn production_schema_failure_reports_cleanup_without_leftovers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    Connection::open(&database)
        .unwrap()
        .execute_batch("create table unsupported(value text)")
        .unwrap();
    let registry = fixture_registry(&data_root, &database);
    fail_next_opened_snapshot_cleanup_for_test();

    let error = refresh_source_backed_generation(&index_root, &registry, fixture_writer_options())
        .unwrap_err();
    let SourceBackedCoordinatorError::RouteScan { source, .. } = error else {
        panic!("unexpected Hermes refresh error: {error:?}");
    };
    assert_eq!(source.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert!(source.detail.contains("cleanup_status=failed"));
    let staging = data_root.join("tmp/provider-sqlite");
    assert!(staging.is_dir());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[test]
fn detailed_progress_reports_backup_inventory_and_changed_session_scan() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_fixture(&database);
    let registry = fixture_registry(&data_root, &database);
    let mut progress = Vec::new();

    let receipt = refresh_source_backed_generation_with_detailed_progress(
        index_root,
        &registry,
        fixture_writer_options(),
        |update| {
            if let Some(current) = update.current_source_progress {
                progress.push(current);
            }
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(receipt.sources.len(), 2);
    assert!(progress.iter().any(|update| {
        matches!(
            update.stage,
            SourceBackedCurrentSourceProgressStage::OnlineBackup
                | SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
        )
    }));
    assert!(progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            && update.logical_rows_scanned == Some(4)
    }));
    assert!(progress.iter().any(|update| {
        update.stage == SourceBackedCurrentSourceProgressStage::LogicalScan
            && update.logical_rows_scanned == Some(2)
    }));
}

fn provider_family_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
    [path.to_path_buf(), path.with_extension("db-wal")]
        .into_iter()
        .filter(|member| member.exists())
        .map(|member| {
            (
                member.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(member).unwrap(),
            )
        })
        .collect()
}

fn provider_directory_names(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}
