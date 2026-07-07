mod support;

use support::*;

#[test]
fn index_status_and_watch_are_read_only_for_missing_store() {
    let temp = tempdir();

    let status = json_output(ctx(&temp).args(["index", "status", "--json"]));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["lexical"]["status"], "missing");
    assert_eq!(status["local_only"], true);
    assert_eq!(status["read_only"], true);
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index status must not initialize the store"
    );

    let stderr =
        failure_stderr(ctx(&temp).args(["index", "watch", "--json", "--interval-seconds", "1"]));
    assert!(stderr.contains("ctx index does not exist yet"), "{stderr}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index watch failure must not initialize the store"
    );
}

#[test]
fn index_wait_lexical_reports_ready_after_import() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));

    let status = json_output(ctx(&temp).args(["index", "status", "--json"]));
    assert_eq!(status["initialized"], true);
    assert_eq!(status["lexical"]["status"], "ready");
    assert!(status["lexical"]["indexed_items"].as_u64().unwrap() > 0);

    let wait = json_output(ctx(&temp).args([
        "index",
        "wait",
        "--lexical",
        "--json",
        "--timeout-seconds",
        "1",
        "--interval-seconds",
        "1",
    ]));
    assert_eq!(wait["schema_version"], 1);
    assert_eq!(wait["status"], "ready");
    assert_eq!(wait["selection"]["lexical"], true);
    assert_eq!(wait["selection"]["semantic"], false);
    assert_eq!(wait["index"]["lexical"]["status"], "ready");
    assert_eq!(wait["local_only"], true);
    assert_eq!(wait["read_only"], true);
}
