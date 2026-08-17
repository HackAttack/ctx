use std::{fs, path::Path};

use ctx_history_capture_model::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind,
};
use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use super::*;
use crate::lifecycle::SourceBackedRouteErrorKind;

#[path = "tests/inventory_mutation_tests.rs"]
mod inventory_mutation;

const PARENT: &str = "parent-session";
const CHILD: &str = "child-session";

#[test]
fn direct_core_projection_is_complete_and_has_no_recursive_ancestry_sql() {
    let production = [
        include_str!("../source_backed.rs"),
        include_str!("projection.rs"),
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

fn automatic_candidate(data_root: &Path, database: &Path) -> HermesSourceCandidate {
    HermesSourceCandidate::automatic(
        data_root,
        crate::ProviderSource {
            provider: CaptureProvider::Hermes,
            path: database.to_path_buf(),
            exists: true,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: crate::ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
    )
    .unwrap()
}

#[test]
fn renamed_profile_control_is_rejected_for_a_different_profile_descriptor() {
    let receipt = HermesRefreshReceipt {
        kind: HERMES_ROUTE_CONTROL_KIND.to_owned(),
        version: HERMES_ROUTE_CONTROL_VERSION,
        profile_source_descriptor: [1; 32],
        database_identity: [2; 32],
        schema_evidence: [3; 32],
        session_rowid: 10,
        message_rowid: 20,
        last_successful_exhaustive_at_ms: 1000,
        exact_due_at_ms: 2000,
        exhaustive_sequence: 1,
        mode: "incremental".to_owned(),
        outcome: "successful".to_owned(),
    };
    let control = serde_json::to_vec(&receipt).unwrap();
    assert_eq!(
        hermes_route_control_exact_due_for_profile(&control, [9; 32], 1500),
        None
    );
    assert_eq!(
        hermes_route_control_exact_due_for_profile(&control, [1; 32], 1500),
        Some(false)
    );
}

#[test]
fn session_source_keys_are_profile_scoped_and_stable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("profiles/alpha/state.db");
    let second = temp.path().join("profiles/beta/state.db");
    create_fixture(&first);
    create_fixture(&second);
    let first_candidate = automatic_candidate(temp.path(), &first);
    let second_candidate = automatic_candidate(temp.path(), &second);

    assert_eq!(
        session_source(&first_candidate, PARENT),
        session_source(&first_candidate, PARENT)
    );
    assert_ne!(
        session_source(&first_candidate, PARENT),
        session_source(&second_candidate, PARENT)
    );
    assert_ne!(
        session_source(&first_candidate, PARENT),
        session_source(&first_candidate, CHILD)
    );
}

fn projected_bodies(
    candidate: &HermesSourceCandidate,
    snapshot: &crate::provider_sources::SqliteSourceReadSnapshot,
    session: &str,
) -> Vec<String> {
    let mut inventory =
        observe_hermes_session_inventory::<crate::registration::tests::NoopLifecycle>(
            candidate,
            snapshot.connection().unwrap(),
            &mut |_| Ok(()),
        )
        .unwrap();
    let leaf = &inventory
        .leaves
        .iter()
        .find(|leaf| leaf.provider_leaf.provider_session_id == session)
        .unwrap()
        .provider_leaf;
    let mut bodies = Vec::new();
    project_hermes_session_snapshot(
        candidate,
        leaf,
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
fn concurrent_commit_cannot_mix_hermes_inventory_and_body_observations() {
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
}
