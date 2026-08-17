use super::*;

#[test]
fn long_lived_mcp_search_recovers_daemon_after_startup() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new();
    write_codex_session(harness.home(), "long lived mcp recovery oracle");
    let generation = harness.setup_wait();
    let daemon = wait_for_daemon(&harness, None);
    let daemon_pid = live_pid(&daemon);

    let mut mcp = harness.mcp_session();
    let initialized = mcp.request(json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "daemon-recovery-test", "version": "0" }
        }
    }));
    assert_eq!(initialized["result"]["serverInfo"]["name"], "ctx");

    let stale = force_unexpected_death(&harness, daemon_pid);
    let searched = mcp.request(json!({
        "jsonrpc": "2.0",
        "id": "search-after-daemon-death",
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "long lived mcp recovery oracle",
                "provider": "codex",
                "limit": 5
            }
        }
    }));
    assert!(
        searched.get("error").is_none()
            && searched["result"]["isError"].as_bool() != Some(true),
        "{searched:#}"
    );
    let recovered = wait_for_daemon(&harness, Some(daemon_pid));
    let recovered_pid = live_pid(&recovered);
    assert_replaced_stale_owner(&harness, &stale, recovered_pid);

    let payload = &searched["result"]["structuredContent"];
    assert_eq!(
        payload["retrieval"]["generation_id"], generation,
        "{searched:#}"
    );
}

#[test]
fn live_readiness_rejoins_while_the_main_scheduler_is_blocked() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new();
    write_codex_session(harness.home(), "blocked scheduler readiness oracle");
    harness.setup_wait();
    harness.json(&["daemon", "disable", "--format=json"]);

    let block = harness
        .root()
        .join(".block-daemon-main-after-ready-for-test");
    let blocked = harness
        .root()
        .join(".daemon-main-blocked-after-ready-for-test");
    fs::write(&block, b"block\n").unwrap();
    let started = harness.json(&["daemon", "enable", "--format=json"]);
    let pid = json_u32(&started, "pid").expect("daemon pid");
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    while !blocked.exists() {
        assert!(
            Instant::now() < deadline,
            "daemon did not reach Ready fence"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let owner = read_lock(harness.root()).expect("daemon owner lock");

    let status_path = harness.root().join("daemon/status.json");
    let mut status = read_json_file(&status_path);
    status["heartbeat_at_ms"] = json!(1);
    fs::write(&status_path, serde_json::to_vec(&status).unwrap()).unwrap();
    fs::remove_file(harness.root().join("daemon/jobs/core-refresh.json")).unwrap();

    let rejoined = harness.json(&["daemon", "enable", "--format=json"]);
    assert_eq!(json_u32(&rejoined, "pid"), Some(pid), "{rejoined:#}");
    assert_eq!(
        read_lock(harness.root()).expect("rejoined owner")["owner_id"],
        owner["owner_id"]
    );

    fs::remove_file(block).unwrap();
    harness.json(&["daemon", "disable", "--format=json"]);
}
