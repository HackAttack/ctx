use super::*;

#[test]
fn mcp_terminal_activity_preserves_exact_server_tool_and_linkage() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let payload = serde_json::json!({
        "type": "mcp_tool_call_end",
        "call_id": "call-terminal",
        "invocation": {
            "server": "source-server",
            "tool": "source-tool",
            "arguments": {"path": "A/../B", "items": [1, 1]}
        },
        "status": "provider::failed",
        "result": {"error": "native failure"}
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-terminal").unwrap())
    );
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
    assert_eq!(invocation.server.as_deref(), Some("source-server"));
    assert_eq!(invocation.tool, "source-tool");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"path": "A/../B", "items": [1, 1]})
        }
    );
    let result = activity.result.unwrap();
    assert_eq!(result.status.as_deref(), Some("provider::failed"));
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"error": "native failure"})
        }
    );

    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": "call-ctx-search",
            "invocation": {
                "server": "ctx",
                "tool": "search",
                "arguments": {"query": "needle"}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": "{\"results\":[]}"}],
                    "isError": false
                }
            }
        }
    });
    let raw = serde_json::to_vec(&exact).unwrap();
    let payload = exact.get("payload").unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    assert_eq!(
        codex_result_discovery_exclusion(&raw, None, true, Some(&activity)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );

    for mutation in ["ordinary", "error", "diagnostic"] {
        let mut control = exact.clone();
        match mutation {
            "ordinary" => {
                control["payload"]["invocation"]["server"] = Value::String("filesystem".to_owned())
            }
            "error" => control["payload"]["result"]["Ok"]["isError"] = Value::Bool(true),
            "diagnostic" => {
                control["payload"]["result"]["Ok"]["warning"] =
                    Value::String("provider warning".to_owned())
            }
            _ => unreachable!(),
        }
        let raw = serde_json::to_vec(&control).unwrap();
        let payload = control.get("payload").unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert_eq!(
            codex_result_discovery_exclusion(&raw, None, true, Some(&activity)),
            None,
            "unexpected exclusion for {mutation} MCP terminal"
        );
    }
}

#[test]
fn mcp_retrieval_exclusion_requires_nonempty_text_payload_and_valid_duration() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": "call-ctx-search-payload",
            "invocation": {
                "server": "ctx",
                "tool": "search",
                "arguments": {"query": "needle"}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": "{\"results\":[]}"}],
                    "isError": false
                }
            }
        }
    });
    let classify = |record: &Value| {
        let raw = serde_json::to_vec(record).unwrap();
        let payload = record.get("payload").unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        codex_result_discovery_exclusion(&raw, None, true, Some(&activity))
    };

    assert_eq!(
        classify(&exact),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut no_duration = exact.clone();
    no_duration["payload"]
        .as_object_mut()
        .unwrap()
        .remove("duration");
    assert_eq!(
        classify(&no_duration),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut structured = exact.clone();
    structured["payload"]["result"]["Ok"]["structuredContent"] = serde_json::json!({"results": []});
    assert_eq!(
        classify(&structured),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );

    for (mutation, invalid) in [
        ("empty-content", serde_json::json!([])),
        (
            "empty-text",
            serde_json::json!([{"type": "text", "text": ""}]),
        ),
        (
            "non-text",
            serde_json::json!([{"type": "image", "data": "AA=="}]),
        ),
        (
            "mixed-content",
            serde_json::json!([
                {"type": "text", "text": "payload"},
                {"type": "image", "data": "AA=="}
            ]),
        ),
    ] {
        let mut control = exact.clone();
        control["payload"]["result"]["Ok"]["content"] = invalid;
        assert_eq!(
            classify(&control),
            None,
            "unexpected exclusion for {mutation}"
        );
    }

    let mut structured_only = exact.clone();
    structured_only["payload"]["result"]["Ok"]
        .as_object_mut()
        .unwrap()
        .remove("content");
    structured_only["payload"]["result"]["Ok"]["structuredContent"] =
        serde_json::json!({"results": []});
    assert_eq!(classify(&structured_only), None);

    for (mutation, invalid) in [
        ("null-duration", Value::Null),
        ("non-object-duration", Value::String("fast".to_owned())),
        ("missing-secs", serde_json::json!({"nanos": 42})),
        (
            "unknown-duration-field",
            serde_json::json!({"secs": 0, "nanos": 42, "warning": true}),
        ),
        (
            "out-of-range-nanos",
            serde_json::json!({"secs": 0, "nanos": 1_000_000_000_u64}),
        ),
    ] {
        let mut control = exact.clone();
        control["payload"]["duration"] = invalid;
        assert_eq!(
            classify(&control),
            None,
            "unexpected exclusion for {mutation}"
        );
    }

    let mut metadata = exact;
    metadata["payload"]["result"]["Ok"]["_meta"] =
        serde_json::json!({"warning": "mixed diagnostic"});
    assert_eq!(classify(&metadata), None);
}

#[test]
fn direct_ctx_retrieval_invocation_excludes_without_losing_activity() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (command, expected) in [
        (
            "ctx search exact-retrieval",
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
        ),
        ("ctx status", None),
        ("git status", None),
    ] {
        let payload = serde_json::json!({
            "type": "function_call",
            "call_id": format!("call-{command}"),
            "name": "exec_command",
            "arguments": {"cmd": command}
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();

        assert_eq!(
            codex_invocation_discovery_exclusion(&payload, &audit, Some(&activity)),
            expected,
            "unexpected classification for {command}"
        );
        assert_eq!(
            activity.invocation.as_ref().unwrap().arguments,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"cmd": command})
            }
        );
    }
}

#[test]
fn linked_ctx_retrieval_result_requires_exact_success_envelope() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let output = concat!(
        "Script completed\n",
        "Process exited with code 0\n",
        "Final output:\n",
        "{\"results\":[]}"
    );
    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": "call-linked",
            "status": "success",
            "output": output
        }
    });
    let classify =
        |record: &Value, linked_invocation_discovery_exclusion: Option<CoreDiscoveryExclusion>| {
            let raw = serde_json::to_vec(record).unwrap();
            let payload = record.get("payload").unwrap();
            let audit = audit_codex_record(&raw).unwrap();
            let activity = codex_result_activity(
                payload.get("call_id").and_then(Value::as_str),
                payload.get("output"),
                payload,
                &audit,
                occurred_at,
            )
            .unwrap();
            codex_result_discovery_exclusion(
                &raw,
                linked_invocation_discovery_exclusion,
                true,
                Some(&activity),
            )
        };

    assert_eq!(
        classify(&exact, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(classify(&exact, None), None);
    let mut legacy = exact.clone();
    legacy["payload"].as_object_mut().unwrap().remove("status");
    legacy["payload"]["output"] = Value::String(
        concat!(
            "Chunk ID: abc123\n",
            "Wall time: 0.1 seconds\n",
            "Process exited with code 0\n",
            "Final output:\n",
            "{\"results\":[]}"
        )
        .to_owned(),
    );
    assert_eq!(
        classify(&legacy, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut failed = exact.clone();
    failed["payload"]["status"] = Value::String("failed".to_owned());
    assert_eq!(
        classify(&failed, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        None
    );
    let mut diagnostic = exact;
    diagnostic["payload"]["stderr"] = Value::String("diagnostic".to_owned());
    assert_eq!(
        classify(
            &diagnostic,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        ),
        None
    );
}

#[test]
fn activity_preserves_exact_mcp_invocation_and_result_channels() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let invocation = serde_json::json!({
        "type": "function_call",
        "call_id": "call-exact",
        "name": "mcp__forge__open",
        "arguments": {
            "command": "  git status  ",
            "path": "./exact",
            "url": "https://example.invalid/p?q=a%20b"
        }
    });
    let raw = serde_json::to_vec(&invocation).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_invocation_activity(&invocation, &audit, occurred_at).unwrap();
    assert_eq!(
        codex_invocation_discovery_exclusion(&invocation, &audit, Some(&activity)),
        None
    );
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-exact").unwrap())
    );
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.protocol, None);
    assert_eq!(invocation.server, None);
    assert_eq!(invocation.tool, "mcp__forge__open");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({
                "command": "  git status  ",
                "path": "./exact",
                "url": "https://example.invalid/p?q=a%20b"
            })
        }
    );

    let provider_result = serde_json::json!({
        "content": ["first", {"status": "failed"}],
        "command": "  git status  "
    });
    let payload = serde_json::json!({"status":"native", "result":provider_result});
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        Some("call-exact"),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    let result = activity.result.unwrap();
    assert_eq!(result.status.as_deref(), Some("native"));
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: payload.get("result").unwrap().clone()
        }
    );
}

#[test]
fn empty_result_string_is_absent_text_with_exact_structured_capture() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let payload = serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-empty",
        "output": ""
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        Some("call-empty"),
        payload.get("output"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();

    let result = activity.result.unwrap();
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: Value::String(String::new())
        }
    );
}

#[test]
fn terminal_outcomes_preserve_literal_status_and_complete_result_content() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (status, expected) in [
        (Some("provider::ok"), Some("provider::ok")),
        (Some("provider::failed"), Some("provider::failed")),
        (None, None),
    ] {
        let mut payload = serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-outcome",
            "output": {"message": "complete provider result"}
        });
        if let Some(status) = status {
            payload["status"] = Value::String(status.to_owned());
        }
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("output"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        let result = activity.result.unwrap();
        assert_eq!(result.status.as_deref(), expected);
        assert_eq!(result.text, ActivityTextCapture::Absent);
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"message": "complete provider result"})
            }
        );
    }
}

#[test]
fn malformed_mcp_identity_abstains_without_losing_valid_result_activity() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for invocation in [
        serde_json::json!({"server": 7, "tool": "read", "arguments": {}}),
        serde_json::json!({"server": "server", "tool": "", "arguments": {}}),
        serde_json::json!({
            "server": "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1),
            "tool": "read",
            "arguments": {}
        }),
    ] {
        let payload = serde_json::json!({
            "type": "mcp_tool_call_end",
            "call_id": "call-malformed-identity",
            "invocation": invocation,
            "result": "valid result survives"
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert!(activity.invocation.is_none());
        assert_eq!(
            activity.result.unwrap().text,
            ActivityTextCapture::Present {
                value: "valid result survives".to_owned()
            }
        );
    }

    let invalid_call = serde_json::json!({
        "type": "function_call_output",
        "call_id": ["not", "a", "string"],
        "output": "unlinked result"
    });
    let raw = serde_json::to_vec(&invalid_call).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    assert!(codex_result_activity(
        invalid_call.get("call_id").and_then(Value::as_str),
        invalid_call.get("output"),
        &invalid_call,
        &audit,
        occurred_at,
    )
    .is_none());
}

#[test]
fn exact_mcp_identity_boundary_is_accepted_and_max_plus_one_abstains() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exact_server = "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
    let exact_tool = "t".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
    let payload = serde_json::json!({
        "type": "mcp_tool_call_end",
        "call_id": "call-boundary",
        "invocation": {
            "server": exact_server,
            "tool": exact_tool,
            "arguments": {}
        },
        "result": "boundary result"
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.server.as_deref(), Some(exact_server.as_str()));
    assert_eq!(invocation.tool, exact_tool);

    for component in ["server", "tool"] {
        let mut oversized = payload.clone();
        oversized["invocation"][component] =
            Value::String("x".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1));
        let raw = serde_json::to_vec(&oversized).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            oversized.get("call_id").and_then(Value::as_str),
            oversized.get("result"),
            &oversized,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert!(activity.invocation.is_none(), "oversized {component}");
        assert!(activity.result.is_some());
    }
}

#[test]
fn duplicate_selectors_withhold_linkage_and_preserve_raw_fact_order() {
    let raw = br#"{"type":"function_call","call_id":"one","call_id":"two","name":"tool","arguments":{"command":" c ","path":" p ","url":" u "}}"#;
    let payload: Value = serde_json::from_slice(raw).unwrap();
    let audit = audit_codex_record(raw).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();
    assert!(activity.provider_call_id.is_none());
    assert!(activity.invocation.is_none());
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (LiteralFactKind::Command, " c "),
            (LiteralFactKind::File, " p "),
            (LiteralFactKind::Url, " u "),
        ]
    );
}
