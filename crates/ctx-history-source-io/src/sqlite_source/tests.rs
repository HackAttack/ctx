use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use rusqlite::{ffi, params, Connection};

use super::snapshot::{
    fail_next_private_directory_cleanup_for_test, fail_next_private_scratch_close_for_test,
    fail_next_private_scratch_open_for_test, fail_next_snapshot_open_for_test,
    fail_next_snapshot_write_enospc_for_test,
    open_root_handle_sqlite_source_snapshot_before_revalidation_for_test,
    open_root_handle_sqlite_source_snapshot_with_limit_for_test,
    open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test,
};
use super::{
    map_revalidation_error, map_revalidation_io_error, open_root_handle_sqlite_source_snapshot,
    retain_sqlite_source_directory_authority, SqliteArtifactKind, SqliteCleanupStatus,
    SqliteFailurePhase, SqliteSourceAccessError, SqliteSourceComponent,
    SqliteSourceDirectoryAuthority, SqliteSourceProgressError, SqliteSourceProgressStage,
    SqliteSourceReadSnapshot, SqliteSourceSnapshotStrategy, SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES,
    SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
};

mod diagnostics;
mod path_safety;
mod scratch;

fn create_database(path: &Path, value: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
}

#[cfg(target_os = "linux")]
fn create_persistent_wal(path: &Path) -> Connection {
    use rusqlite::config::DbConfig;

    let connection = Connection::open(path).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES ('from-wal')", [])
        .unwrap();
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    connection
}

fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
    retain_parent_in_data_root(crate::test_provider_sqlite_data_root(), path)
}

fn retain_parent_in_data_root(data_root: &Path, path: &Path) -> SqliteSourceDirectoryAuthority {
    fs::create_dir_all(data_root).unwrap();
    let parent = File::open(path).unwrap();
    retain_sqlite_source_directory_authority(data_root, &parent, path).unwrap()
}

fn read_values(snapshot: &SqliteSourceReadSnapshot) -> Vec<String> {
    snapshot
        .connection()
        .unwrap()
        .prepare("SELECT body FROM messages ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

fn staging_entries(data_root: &Path) -> usize {
    let staging = data_root.join("tmp/provider-sqlite");
    match fs::read_dir(staging) {
        Ok(entries) => entries.count(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => panic!("reading SQLite staging root: {error}"),
    }
}

fn physical_error(error: &SqliteSourceAccessError) -> &SqliteSourceAccessError {
    match error {
        SqliteSourceAccessError::Diagnosed { source, .. }
        | SqliteSourceAccessError::ProviderContentCorruption { source } => physical_error(source),
        error => error,
    }
}

#[test]
fn stable_copy_is_one_private_snapshot_and_never_writes_provider_files() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "stable");
    fs::write(temp.path().join("unrelated"), b"unchanged").unwrap();
    let before = directory_file_bytes(temp.path());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    assert_eq!(read_values(&snapshot), ["stable"]);
    assert_eq!(staging_entries(data_root.path()), 1);
    let copied = snapshot.copied_bytes();
    assert_eq!(authority.snapshot_counters().source_bytes_copied(), copied);
    assert!(authority.snapshot_counters().max_route_scratch_bytes() >= copied);
    snapshot.finish().unwrap();

    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(directory_file_bytes(temp.path()), before);
}

#[cfg(target_os = "linux")]
#[test]
fn active_wal_retains_one_family_copy_under_one_aggregate_limit() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let before = directory_file_bytes(temp.path());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert_eq!(staging_entries(data_root.path()), 1);
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    let counters = authority.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.max_active_snapshots(), 1);
    assert!(counters.max_route_scratch_bytes() >= snapshot.copied_bytes());
    assert!(counters.max_route_scratch_bytes() <= SQLITE_SNAPSHOT_MAX_TOTAL_BYTES);
    snapshot.finish().unwrap();

    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(directory_file_bytes(temp.path()), before);
    drop(writer);
}

#[cfg(target_os = "linux")]
#[test]
fn active_source_family_contract_sqlite_keeps_a_pinned_view_and_fails_changed_writer_generation() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();

    writer
        .execute("INSERT INTO messages (body) VALUES ('later')", [])
        .unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert!(matches!(
        snapshot.seal(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert!(!snapshot_directory.exists());
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.terminal_fences(), 0);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);

    let replacement =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&replacement), ["from-wal", "later"]);
    replacement.finish().unwrap();
}

#[test]
fn near_limit_rejection_happens_before_any_scratch_write() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "limit");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let error = open_root_handle_sqlite_source_snapshot_with_limit_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        database_bytes - 1,
    )
    .unwrap_err();

    assert!(error.is_systemic_resource_failure());
    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(authority.snapshot_counters().source_bytes_copied(), 0);
}

#[test]
fn free_space_headroom_rejection_happens_before_any_scratch_write() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "headroom");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    super::override_next_scratch_available_space_for_test(
        database_bytes + SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES - 1,
    );

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    assert!(matches!(
        error,
        SqliteSourceAccessError::InsufficientScratchSpace { .. }
            | SqliteSourceAccessError::Diagnosed { .. }
    ));
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn injected_enospc_cleans_the_single_private_directory() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "enospc");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_snapshot_write_enospc_for_test();

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    assert!(error.is_systemic_resource_failure());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn progress_cancellation_cleans_a_partial_family_copy() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE messages(body TEXT NOT NULL);
             INSERT INTO messages VALUES ('large');
             CREATE TABLE padding(payload BLOB NOT NULL);
             INSERT INTO padding VALUES (zeroblob(10485760));",
        )
        .unwrap();
    drop(connection);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let mut calls = 0;

    let error = authority
        .open_stable_snapshot_with_progress(OsStr::new("provider.sqlite"), |progress| {
            assert_eq!(progress.stage, SqliteSourceProgressStage::SourceFamilyCopy);
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(
        error,
        SqliteSourceProgressError::Progress("cancelled")
    ));
    assert!(calls > 1);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn progress_cancellation_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE messages(body TEXT NOT NULL);
             INSERT INTO messages VALUES ('large');
             CREATE TABLE padding(payload BLOB NOT NULL);
             INSERT INTO padding VALUES (zeroblob(10485760));",
        )
        .unwrap();
    drop(connection);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let mut calls = 0;
    fail_next_private_directory_cleanup_for_test();

    let error = authority
        .open_stable_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| {
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    match error {
        SqliteSourceProgressError::ProgressAndFinalization {
            primary,
            finalization,
        } => {
            assert_eq!(primary, "cancelled");
            assert!(finalization
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
            assert_eq!(
                finalization.diagnostic().unwrap().cleanup,
                SqliteCleanupStatus::Failed
            );
        }
        other => panic!("expected cancellation plus cleanup failure, got {other:?}"),
    }
    assert!(calls > 1);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn snapshot_open_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "open failure");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_snapshot_open_for_test();
    fail_next_private_directory_cleanup_for_test();

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary
                .to_string()
                .contains("opening the ctx-owned provider snapshot"));
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected open plus cleanup failure, got {other:?}"),
    }
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn acquisition_revalidation_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "revalidation failure");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_private_directory_cleanup_for_test();

    let error = open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        || {
            let mut source = fs::OpenOptions::new().append(true).open(&database).unwrap();
            use std::io::Write as _;
            source.write_all(&[0]).unwrap();
            source.sync_all().unwrap();
        },
    )
    .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary.is_source_changed());
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected revalidation plus cleanup failure, got {other:?}"),
    }
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn corrupt_source_copy_fails_closed_and_cleans_scratch() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("provider.sqlite"),
        b"not a sqlite database",
    )
    .unwrap();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let result = authority.open_stable_snapshot(OsStr::new("provider.sqlite"));

    assert!(result.is_err());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn source_race_after_database_copy_fails_closed_and_cleans_scratch() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let result = open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        || {
            Connection::open(&database)
                .unwrap()
                .pragma_update(None, "user_version", 7)
                .unwrap();
        },
    );

    assert!(matches!(result, Err(error) if error.is_source_changed()));
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn finish_is_mandatory_observable_and_revalidates_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let admitted = temp.path().join("admitted.sqlite");
    create_database(&database, "expected");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let terminal = snapshot.terminal_revalidator();
    fs::rename(&database, &admitted).unwrap();
    create_database(&database, "replacement");

    assert!(snapshot.finish().is_err());
    assert!(terminal().is_err());
    assert_eq!(authority.snapshot_counters().terminal_fences(), 0);
    assert_eq!(authority.snapshot_counters().unfinished_drops(), 0);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn abort_and_unfinished_drop_are_distinct_observable_paths() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "observable");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap()
        .abort()
        .unwrap();
    drop(
        authority
            .open_stable_snapshot(OsStr::new("provider.sqlite"))
            .unwrap(),
    );

    let counters = authority.snapshot_counters();
    assert_eq!(counters.explicit_aborts(), 1);
    assert_eq!(counters.unfinished_drops(), 1);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn retained_copy_and_ordering_database_share_one_exact_route_bound() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "aggregate");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let aggregate_limit = database_bytes + 128 * 1024;
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let snapshot = open_root_handle_sqlite_source_snapshot_with_limit_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        aggregate_limit,
    )
    .unwrap();

    snapshot
        .with_private_scratch_database(
            "aggregate-",
            128 * 1024,
            |scratch, _| -> Result<(), SqliteSourceAccessError> {
                scratch
                    .execute_batch(
                        "CREATE TABLE ordered(value BLOB NOT NULL);
                         INSERT INTO ordered VALUES (zeroblob(32768));",
                    )
                    .map_err(|source| {
                        SqliteSourceAccessError::private_scratch_sqlite(
                            "writing aggregate scratch fixture",
                            source,
                        )
                    })?;
                Ok(())
            },
        )
        .unwrap();
    let counters = authority.snapshot_counters();
    assert!(counters.max_route_scratch_bytes() > database_bytes);
    assert!(counters.max_route_scratch_bytes() <= aggregate_limit);
    assert_eq!(counters.scratch_admissions(), 2);
    snapshot.finish().unwrap();
}
