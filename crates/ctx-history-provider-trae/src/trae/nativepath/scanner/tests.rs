use std::{cell::Cell, ffi::OsString, fs, path::Path};

use rusqlite::{config::DbConfig, params, Connection};

use super::{acquire_source, TraeFrontier, TraeScanner, TraeSqliteDatabase};
use crate::CaptureError;

#[test]
fn primary_key_itemtable_remains_importable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let source = temp.path().join("state.vscdb");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
            params![
                crate::TRAE_CHAT_KEYS[0],
                r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
            ],
        )
        .unwrap();
    drop(connection);

    let authority = acquire_source(
        data_root.path(),
        &source,
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    )
    .unwrap();
    let mut scanner = TraeScanner::new(&authority, TraeFrontier::default());
    let page = scanner.next_page().unwrap().unwrap();
    assert_eq!(page.core.len(), 1);
    assert!(page.rejections.is_empty());
}

#[test]
fn duplicate_known_itemtable_keys_are_typed_invalid_payload() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let source = temp.path().join("state.vscdb");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute("CREATE TABLE ItemTable ([key] TEXT, value TEXT)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2), (?1, ?3)",
            params![
                crate::TRAE_CHAT_KEYS[0],
                r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                r#"{"list":[]}"#,
            ],
        )
        .unwrap();
    drop(connection);

    let error = match acquire_source(
        data_root.path(),
        &source,
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    ) {
        Ok(_) => panic!("duplicate known Trae keys must be rejected before import"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CaptureError::InvalidPayload(detail)
            if detail == "Trae ItemTable key `memento/icube-ai-agent-storage` appears 2 times"
    ));
}

#[test]
fn malformed_sibling_key_is_isolated_as_a_record_rejection() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let source = temp.path().join("state.vscdb");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2), (?3, ?4)",
            params![
                crate::TRAE_CHAT_KEYS[0],
                r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                crate::TRAE_CHAT_KEYS[1],
                "invalid JSON",
            ],
        )
        .unwrap();
    drop(connection);

    let authority = acquire_source(
        data_root.path(),
        &source,
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    )
    .unwrap();
    let mut scanner = TraeScanner::new(&authority, TraeFrontier::default());
    let page = scanner.next_page().unwrap().unwrap();
    assert_eq!(page.core.len(), 1);
    assert_eq!(page.rejections.len(), 1);
    assert!(page.rejections[0].error.contains("contains invalid JSON"));
}

#[test]
fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source = temp.path().join("trae.sqlite");
    let attacker = temp.path().join("attacker.sqlite");
    let admitted = temp.path().join("admitted.sqlite");
    create_database(&source, "main");
    create_database(&attacker, "attacker");
    persist_wal_row(&source, "from-wal");
    let before_read = persistent_directory_snapshot(temp.path());

    let (database, opened_value) = TraeSqliteDatabase::open(
        crate::test_provider_sqlite_data_root(),
        &source,
        read_latest,
    )
    .unwrap();
    assert_eq!(opened_value, "from-wal");
    assert!(database.evidence().wal_length().is_some());
    assert!(database.evidence().shared_memory_length().is_some());
    assert_eq!(database.read(&source, read_latest).unwrap(), "from-wal");
    assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

    fs::rename(&source, &admitted).unwrap();
    fs::rename(&attacker, &source).unwrap();
    let before_rejected_read = persistent_directory_snapshot(temp.path());
    let queried = Cell::new(false);
    let result = database.read(&source, |_| -> crate::Result<()> {
        queried.set(true);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!queried.get());
    assert_eq!(
        persistent_directory_snapshot(temp.path()),
        before_rejected_read
    );
}

fn create_database(path: &Path, value: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
}

fn persist_wal_row(path: &Path, value: &str) {
    let writer = Connection::open(path).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    assert!(path.with_file_name("trae.sqlite-wal").exists());
    assert!(path.with_file_name("trae.sqlite-shm").exists());
}

fn read_latest(connection: &Connection) -> crate::Result<String> {
    Ok(connection.query_row(
        "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?)
}

fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-shm")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_os_string(),
                fs::read(path).unwrap(),
            )
        })
        .collect()
}
