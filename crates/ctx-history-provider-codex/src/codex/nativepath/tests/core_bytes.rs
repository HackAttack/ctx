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

fn copied_child_source(path: &Path, child: &str, _parent: &str) -> CodexCatalogSource {
    let mut catalog = catalog_session(path, child);
    catalog.agent_type = AgentType::Subagent;
    let mut source = discover_codex_catalog_sources(&[catalog]).sources.remove(0);
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

fn lineage_snapshot(
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
    payload_session_id: &str,
) -> (usize, String, CollectingSink) {
    let mut payload = json!({
        "id": native_session_id,
        "session_id": payload_session_id,
        "timestamp": "2026-01-01T00:00:00Z",
        "cwd": "/workspace",
        "source": "cli"
    });
    if let Some(parent) = parent_native_session_id {
        match relationship {
            SessionRelationshipKind::Delegated => {
                payload["source"] = json!({
                    "subagent": {"thread_spawn": {"parent_thread_id": parent}}
                });
                payload["parent_thread_id"] = json!(parent);
            }
            SessionRelationshipKind::Forked => payload["forked_from_id"] = json!(parent),
            SessionRelationshipKind::ResumedFrom => {
                payload["history_base"] = json!({"thread_id": parent});
            }
            relationship => panic!("unsupported lineage oracle relationship {relationship:?}"),
        }
    }
    let contents = [
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": payload
        })),
        message("assistant", "lineage byte oracle"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    snapshot(discover_one(&path, native_session_id))
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

    let repeated_touch = "/workspace/raw\\segment/\"quoted name\".rs";
    let touched_paths = [
        session_meta("oracle-touched-paths"),
        repository_call(
            "touched-paths-call",
            &format!(
                "*** Begin Patch\n*** Update File: {repeated_touch}\n@@\n*** Delete File: {repeated_touch}\n*** Add File: relative/雪 \\ raw.rs\n*** End Patch"
            ),
        ),
    ]
    .concat();
    let (_touched_paths_temp, touched_paths_path) = write_source(&touched_paths);

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
        snapshot(discover_one(&touched_paths_path, "oracle-touched-paths")),
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
    assert_eq!(
        cases[3].2.rows[0].metadata["codex_native_activity"]["touched_paths"],
        serde_json::json!([repeated_touch, repeated_touch, "relative/雪 \\ raw.rs",])
    );
    assert!(cases[5]
        .2
        .pages
        .iter()
        .any(|(_, bytes)| *bytes > MAX_CODEX_PAGE_BYTES));
    assert_eq!(cases[5].2.pages.first().map(|page| page.0), Some(1));
    assert!(cases[5].2.pages.len() > 1);
    assert!(cases[6].2.pages.len() > 1);
    // The original cases were generated in an exact-base 2ae70373f control by
    // moving every legacy row-page entry through codex_core_record. The touched
    // path case was frozen before removing the duplicate draft vector. Every
    // case hashes each length and encode_stored byte sequence under the same
    // domain as snapshot().
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
            1,
            "2d6037b5e9991de578648b92dc7c774268e272648c064e2571ef0dcd1decbeb6",
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

#[test]
fn source_authoritative_lineage_preserves_exact_core_record_bytes() {
    let parent = "019fb100-0000-7000-8000-000000000200";
    let cases = [
        lineage_snapshot(
            "019fb100-0000-7000-8000-000000000201",
            SessionRelationshipKind::Root,
            None,
            parent,
        ),
        lineage_snapshot(
            "019fb100-0000-7000-8000-000000000202",
            SessionRelationshipKind::Delegated,
            Some(parent),
            "unrelated-advisory-delegated",
        ),
        lineage_snapshot(
            "019fb100-0000-7000-8000-000000000203",
            SessionRelationshipKind::Forked,
            Some(parent),
            "unrelated-advisory-forked",
        ),
        lineage_snapshot(
            "019fb100-0000-7000-8000-000000000204",
            SessionRelationshipKind::ResumedFrom,
            Some(parent),
            "unrelated-advisory-resumed",
        ),
    ];
    for (index, case) in cases.iter().enumerate() {
        let record = &case.2.rows[0];
        assert_eq!(
            record.provider_session_id.as_deref(),
            Some(match index {
                0 => "019fb100-0000-7000-8000-000000000201",
                1 => "019fb100-0000-7000-8000-000000000202",
                2 => "019fb100-0000-7000-8000-000000000203",
                3 => "019fb100-0000-7000-8000-000000000204",
                _ => unreachable!(),
            })
        );
        if index == 0 {
            assert_eq!(record.parent_session_id, None);
            assert_eq!(record.root_session_id, record.session_id);
        } else {
            assert!(record.parent_session_id.is_some());
            assert_eq!(record.parent_session_id, Some(record.root_session_id));
        }
    }
    let expected = [
        (
            1,
            "477ab0d044267e464cea7569e672f22395a2d9288acdf9e8388fe0277f18d649",
        ),
        (
            1,
            "207d283c827f87b525548fd27ba0391992b12707694d5efd9b1d1de1fea6c5fe",
        ),
        (
            1,
            "99c7efa4a3a244e0272d4f5cab5e05f93ffc1d619667d9530dd020a4e1cfb3d4",
        ),
        (
            1,
            "1ee313d3747a3356c0196b517572d65a483082dc73e9ab94027ea2d0bd80d3e7",
        ),
    ];
    for (case, expected) in cases.iter().zip(expected) {
        assert_eq!((case.0, case.1.as_str()), expected);
    }
}
