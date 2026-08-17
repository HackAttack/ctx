use super::*;
use crate::provider_sources::fail_next_opened_snapshot_cleanup_for_test;

#[test]
fn central_schema_error_runs_cleanup_and_preserves_both_failures() {
    let project = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("data/v2-sessions")).unwrap();
    Connection::open(project.path().join("data/v2.db"))
        .unwrap()
        .execute_batch("CREATE TABLE wrong(value TEXT);")
        .unwrap();
    fail_next_opened_snapshot_cleanup_for_test();

    let error = match NanoClawSourceBackedProject::open(data_root.path(), project.path()) {
        Ok(_) => panic!("invalid NanoClaw central schema unexpectedly opened"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(rendered.contains("missing required sessions table"));
    assert!(rendered.contains("injected SQLite snapshot cleanup failure"));
    assert_eq!(fs::read_dir(data_root.path()).unwrap().count(), 0);
}
