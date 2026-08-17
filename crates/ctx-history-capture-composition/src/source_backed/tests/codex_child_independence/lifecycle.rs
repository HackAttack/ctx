use super::*;

fn mcp_terminal(
    call_id: &str,
    server: &str,
    tool: &str,
    status: &str,
    result: &str,
) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": {
                "server": server,
                "tool": tool,
                "arguments": {"path": format!("/{call_id}")}
            },
            "status": status,
            "result": result
        }
    })
}

#[test]
fn retired_semantic_v2_checkpoint_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("retiredv2", retired_semantic_v2_checkpoint);
}

#[test]
fn malformed_semantic_checkpoint_key_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("malformedkey", |_| TypedKey::U64(2));
}

#[test]
fn overlapping_automatic_and_explicit_routes_keep_selected_generation_ownership() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000013";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("automatic-explicit-cold-marker")],
    );

    let mut registry = register_tree(&[&sessions]);
    add_explicit_route(&mut registry, &path);
    let automatic_route = route_identity(&registry, &sessions);
    let explicit_route = route_identity(&registry, &path);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("automatic-explicit-cold-marker", 8)
            .unwrap()
            .len(),
        1
    );

    append_event(&path, message("explicit-only-append-marker"));
    let explicit = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [explicit_route],
    )
    .unwrap();
    assert!(explicit.failed_routes.is_empty());
    assert_eq!(explicit.sources.len(), 1);

    append_event(&path, message("automatic-only-append-marker"));
    let automatic = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [automatic_route],
    )
    .unwrap();
    assert!(automatic.failed_routes.is_empty());
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    for marker in [
        "automatic-explicit-cold-marker",
        "explicit-only-append-marker",
        "automatic-only-append-marker",
    ] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record.content.normalized_body.as_deref() == Some(marker))
                .count(),
            1,
            "missing or duplicated {marker}"
        );
    }
}

#[test]
fn codex_mcp_activity_append_replay_preserves_stable_ids_and_exact_content() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let cold_root = temp.path().join("cold");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000014";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [mcp_terminal(
            "call-first",
            "server-first",
            "tool-first",
            "provider::ok",
            "first activity result",
        )],
    );
    let registry = register_tree(&[&sessions]);

    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    let initial_record = records_for(&initial, native_session_id)
        .into_iter()
        .find(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.provider_call_id.as_ref())
                == Some(&TypedKey::Utf8("call-first".to_owned()))
        })
        .unwrap();
    let initial_event_id = initial_record.event_id;
    let initial_activity = initial_record.content.activity.clone().unwrap();
    assert_eq!(initial_record.parser_revision, CURRENT_PARSER_REVISION);
    drop(initial);

    append_event(
        &path,
        mcp_terminal(
            "call-second",
            "server-second",
            "tool-second",
            "provider::failed",
            "second activity result",
        ),
    );
    let appended =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    assert!(appended.logical_source_failures.is_empty());
    let appended_generation = appended.commit.generation_id.clone();

    let appended_index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&appended_index, native_session_id);
    let first = records
        .iter()
        .find(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.provider_call_id.as_ref())
                == Some(&TypedKey::Utf8("call-first".to_owned()))
        })
        .unwrap();
    assert_eq!(first.event_id, initial_event_id);
    assert_eq!(first.content.activity.as_ref(), Some(&initial_activity));
    let second = records
        .iter()
        .find(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.provider_call_id.as_ref())
                == Some(&TypedKey::Utf8("call-second".to_owned()))
        })
        .unwrap();
    let second_activity = second.content.activity.as_ref().unwrap();
    let invocation = second_activity.invocation.as_ref().unwrap();
    assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
    assert_eq!(invocation.server.as_deref(), Some("server-second"));
    assert_eq!(invocation.tool, "tool-second");
    let result = second_activity.result.as_ref().unwrap();
    assert_eq!(result.status.as_deref(), Some("provider::failed"));
    assert_eq!(
        result.text,
        ctx_history_core::ActivityTextCapture::Present {
            value: "second activity result".to_owned()
        }
    );
    let appended_snapshot =
        source_snapshot(&appended_index, native_session_id, "second activity result");
    drop(appended_index);

    let replay =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(replay.commit.generation_id, appended_generation);

    refresh_source_backed_generation(&cold_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&cold_root).unwrap();
    assert_eq!(
        source_snapshot(&cold, native_session_id, "second activity result"),
        appended_snapshot
    );
}
