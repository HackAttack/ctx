use super::*;

fn snapshot(source: CodexCatalogSource) -> (usize, String, CollectingSink) {
    let (_, sink) = scan_collect(source);
    let mut hasher = Sha256::new();
    hasher.update(b"ctx/codex/core-record-byte-oracle/v1\0");
    for row in &sink.rows {
        let bytes = row.encode_stored().unwrap();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (sink.rows.len(), digest, sink)
}

fn copied_child_source(path: &Path, child: &str, parent: &str) -> CodexCatalogSource {
    let mut catalog = catalog_session(path, child);
    catalog.parent_external_session_id = Some(parent.to_owned());
    catalog.session_relationship = SessionRelationshipKind::Forked;
    catalog.agent_type = AgentType::Subagent;
    let mut source = discover_codex_catalog_sources(&[catalog]).sources.remove(0);
    source.catalog_root_native_session_id = Some(parent.to_owned());
    let opened = open_provider_source_file(path).unwrap();
    source.catalog_prefix_sha256 = Some(
        super::super::reader::opened_file_prefix_sha256(
            opened.file(),
            source.catalog_observation.len,
        )
        .unwrap(),
    );
    source
}

fn repository_call(call_id: &str, command: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": {"cmd": command}
        }
    }))
}

#[test]
fn legacy_bridge_and_direct_core_record_bytes_match_edge_fixture_oracles() {
    let normal = [
        session_meta("oracle-normal"),
        message("user", "normal user"),
        reasoning("normal reasoning"),
        tool_call("normal-call"),
        tool_output("normal-call", "normal output"),
    ]
    .concat();
    let (_normal_temp, normal_path) = write_source(&normal);

    let child = "019fb100-0000-7000-8000-000000000102";
    let parent = "019fb100-0000-7000-8000-000000000101";
    let child_contents = [
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": child,
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli",
                "forked_from_id": parent
            }
        })),
        repository_call(
            "copied-repository-call",
            "git commit -m exact && git rev-parse --verify HEAD",
        ),
        successful_tool_output(
            "copied-repository-call",
            "0123456789abcdef0123456789abcdef01234567",
        ),
    ]
    .concat();
    let (_child_temp, child_path) = write_source(&child_contents);

    let repository_mcp = [
        session_meta("oracle-repository-mcp"),
        repository_call("repository-call", "git status --short"),
        successful_tool_output("repository-call", " M repository.txt"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "oracle-mcp",
                "invocation": {"server": "oracle", "tool": "read", "arguments": {"path": "/workspace/repository.txt"}},
                "duration": {"secs": 0, "nanos": 7},
                "result": {"Ok": {"content": [{"type": "text", "text": "mcp result"}], "isError": false}}
            }
        })),
    ]
    .concat();
    let (_repository_temp, repository_path) = write_source(&repository_mcp);

    let malformed = [
        session_meta("oracle-malformed"),
        "{malformed json}\n".to_owned(),
        message("assistant", "after malformed"),
        jsonl(json!({"timestamp": "bad", "type": "response_item", "payload": {"type": "message"}})),
        message("user", "after malformed retained"),
    ]
    .concat();
    let (_malformed_temp, malformed_path) = write_source(&malformed);

    let oversized = [
        session_meta("oracle-singleton"),
        message("assistant", "rollback-prefix"),
        message(
            "user",
            &format!("{}singleton-tail", "x".repeat(MAX_CODEX_PAGE_BYTES + 1024)),
        ),
    ]
    .concat();
    let (_oversized_temp, oversized_path) = write_source(&oversized);

    let mut rollback = session_meta("oracle-rollback");
    for index in 0..=MAX_CODEX_PAGE_ROWS {
        rollback.push_str(&message("assistant", &format!("rollback-{index}")));
    }
    let (_rollback_temp, rollback_path) = write_source(&rollback);

    let cases = [
        snapshot(discover_one(&normal_path, "oracle-normal")),
        snapshot(copied_child_source(&child_path, child, parent)),
        snapshot(discover_one(&repository_path, "oracle-repository-mcp")),
        snapshot(discover_one(&malformed_path, "oracle-malformed")),
        snapshot(discover_one(&oversized_path, "oracle-singleton")),
        snapshot(discover_one(&rollback_path, "oracle-rollback")),
    ];
    assert!(cases[1].2.rows.iter().all(|row| {
        row.session_relationship == SessionRelationshipKind::Forked
            && row.parent_session_id.is_some()
            && row.root_session_id != row.session_id
    }));
    assert!(cases[1].2.rows.iter().any(|row| matches!(
        row.event_origin,
        ctx_history_core::EventOrigin::CopiedFromAncestor { .. }
    )));
    assert!(cases[2]
        .2
        .rows
        .iter()
        .any(|row| row.mcp_tool_call.is_some()));
    assert!(cases[4]
        .2
        .pages
        .iter()
        .any(|(_, bytes)| *bytes > MAX_CODEX_PAGE_BYTES));
    assert_eq!(cases[4].2.pages.first().map(|page| page.0), Some(1));
    assert!(cases[4].2.pages.len() > 1);
    assert!(cases[5].2.pages.len() > 1);
    // Generated in an exact-base 2ae70373f control by moving every legacy
    // row-page entry through codex_core_record, then hashing each length and
    // encode_stored byte sequence under the same domain as snapshot().
    let expected = [
        (
            4,
            "9c698ad8ec75dd9097a4c97f34ce7bb2711263402e9ebe20f713a169834f3228",
        ),
        (
            2,
            "e51fcd5262ae8641d32cbc508a0a1053cd1372442769a3e0195cae14e3a4b262",
        ),
        (
            3,
            "81ed0c7880191e75d3c950918869c2d607dd8921d03aae0d85bcbe1ea0c5ad46",
        ),
        (
            2,
            "461ed1e416554bba32fa19fc2f7b528513d5b5d1ecd027802c7be3cfc86911b5",
        ),
        (
            2,
            "40e0ef126c997bc485ea44e65d4757048518e85fdfc0a17ce2139ac5fb3d0aac",
        ),
        (
            65,
            "5247a5095e412551ca778daad1b3309ee2baeb70b016b8786609a42fb1c7b449",
        ),
    ];
    for (case, expected) in cases.iter().zip(expected) {
        assert_eq!((case.0, case.1.as_str()), expected);
    }
}
