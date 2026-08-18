use super::*;

fn document() -> PreparedDocument {
    PreparedDocument {
        metadata: serde_json::Value::Null,
        context_branch: None,
        messages: Vec::new(),
        provider_session_id: "session".to_owned(),
        parent_provider_session_id: None,
        started_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        cwd: None,
        initial_failure_count: 0,
    }
}

fn projected_record(raw_message: &serde_json::Value, document: &PreparedDocument) -> CoreRecord {
    let provider_session_id = "session".to_owned();
    let source_key = rovodev_source_key(&provider_session_id).unwrap();
    let session_id = rovodev_session_identity(&source_key, &provider_session_id).unwrap();
    let message_id = explicit_message_id(raw_message).unwrap().to_owned();
    let bound = RovoDevBoundDocument {
        source_key,
        provider_session_id,
        session_id,
        parent_session_id: None,
        unique_message_ids: HashSet::from([message_id]),
    };
    let event = project_message(raw_message, 0, document).unwrap().unwrap();
    core_record(&bound, [9; 32], document, raw_message, 0, event).unwrap()
}

#[test]
fn typed_tool_results_keep_exact_status_strings_and_large_bodies() {
    for status in [Some("SUCCESS"), Some("failure"), None] {
        let mut part = serde_json::json!({
            "kind": "tool_result",
            "tool_use_id": "call-1",
            "content": format!("complete-{status:?}"),
        });
        if let Some(status) = status {
            part["status"] = serde_json::json!(status);
        }
        let message = serde_json::json!({"role": "tool", "parts": [part]});
        let projected = project_message(&message, 0, &document()).unwrap().unwrap();
        assert_eq!(projected.body, format!("complete-{status:?}"));
        let output = projected.output.unwrap();
        assert_eq!(output.call_id.as_deref(), Some("call-1"));
    }

    let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
    let message = serde_json::json!({
        "role": "tool",
        "parts": [{"kind": "tool_result", "content": large}],
    });
    assert!(serde_json::to_vec(&message).unwrap().len() > 8 * 1024 * 1024);
    let projected = project_message(&message, 0, &document()).unwrap().unwrap();
    assert_eq!(projected.body.len(), 9 * 1024 * 1024 + 4);
    assert!(projected.body.ends_with("tail"));

    let ambiguous = serde_json::json!({
        "role": "tool",
        "parts": [{
            "kind": "tool_result",
            "content": "one",
            "output": "two",
        }],
    });
    let projected = project_message(&ambiguous, 0, &document())
        .unwrap()
        .unwrap();
    assert!(projected.body.contains("\"content\":\"one\""));
    assert!(projected.body.contains("\"output\":\"two\""));
    assert!(projected.output.unwrap().capture_unavailable);
}

#[test]
fn rovodev_conflicting_id_name_and_argument_aliases_abstain_without_dropping_records() {
    let conflicting_output = serde_json::json!({
        "role": "tool",
        "tool_use_id": "first-call",
        "call_id": "second-call",
        "content": "retained exact result",
    });
    let projected = project_message(&conflicting_output, 0, &document())
        .unwrap()
        .unwrap();
    assert_eq!(projected.body, "retained exact result");
    assert!(projected.output.unwrap().call_id.is_none());

    let names = serde_json::json!({
        "name": "first_tool",
        "nested": {"tool": "second_tool"},
    });
    assert_eq!(
        known_message_string_field(&names, &["name", "tool"]),
        Some("first_tool".to_owned())
    );
    assert_eq!(
        known_message_string_field(
            &serde_json::json!({"name": "exact_tool", "parts": [{"tool": "exact_tool"}]}),
            &["name", "tool"]
        ),
        Some("exact_tool".to_owned())
    );
    assert_eq!(
        known_message_string_field(
            &serde_json::json!({"name": "exact_tool", "metadata": {"tool": "decoy"}}),
            &["name", "tool"]
        ),
        Some("exact_tool".to_owned())
    );

    assert_eq!(
        known_message_json_alias_capture(
            &serde_json::json!({"arguments": {"x": 1}, "input": {"x": 2}}),
            &["arguments", "args", "input", "parameters"]
        ),
        ActivityJsonCapture::Unavailable
    );
    assert_eq!(
        rovodev_string_alias(
            &serde_json::json!({"status": "first", "state": "second"}),
            &["status", "state", "outcome"]
        ),
        None
    );
}

#[test]
fn rovodev_invalid_optional_facts_abstain_without_changing_identity_or_payload() {
    let raw_message = serde_json::json!({
        "id": "message-1",
        "role": "assistant",
        "content": "retained exact body",
    });
    let mut valid_document = document();
    valid_document.metadata = serde_json::json!({"branch": "main"});
    valid_document.cwd = Some("/workspace".to_owned());
    let valid = projected_record(&raw_message, &valid_document);
    assert_eq!(valid.content.activity.as_ref().unwrap().facts.len(), 2);

    let mut invalid_document = document();
    invalid_document.metadata = serde_json::json!({"branch": ""});
    invalid_document.cwd = Some("x".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES + 1));
    let invalid = projected_record(&raw_message, &invalid_document);

    assert_eq!(invalid.event_id, valid.event_id);
    assert_eq!(invalid.native_event_id, valid.native_event_id);
    assert_eq!(invalid.event_sequence, valid.event_sequence);
    assert_eq!(
        invalid.content.structured_content.as_ref(),
        Some(&raw_message)
    );
    assert!(invalid.content.activity.is_none());
    invalid.validate_contract().unwrap();
}

#[test]
fn rovodev_invalid_optional_tool_metadata_abstains_without_empty_activity() {
    let oversized_metadata = "x".repeat(1024 * 1024);
    for raw_message in [
        serde_json::json!({
            "id": "message-1",
            "role": "tool",
            "tool_use_id": "",
            "content": "retained exact output",
        }),
        serde_json::json!({
            "id": "message-1",
            "role": "tool_use",
            "tool_use_id": oversized_metadata.clone(),
            "name": "read_file",
            "content": "retained exact invocation",
        }),
        serde_json::json!({
            "id": "message-1",
            "role": "tool_use",
            "tool_use_id": "call-1",
            "name": oversized_metadata.clone(),
            "content": "retained exact invocation",
        }),
    ] {
        let record = projected_record(&raw_message, &document());
        assert_eq!(
            record.content.structured_content.as_ref(),
            Some(&raw_message)
        );
        assert!(record.content.activity.is_none());
        record.validate_contract().unwrap();
    }

    let raw_output = serde_json::json!({
        "id": "message-1",
        "role": "tool",
        "tool_use_id": "call-1",
        "status": oversized_metadata,
        "content": "retained exact output",
    });
    let record = projected_record(&raw_output, &document());
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("call-1".to_owned()))
    );
    assert_eq!(activity.result.as_ref().unwrap().status, None);
    assert_eq!(
        record.content.structured_content.as_ref(),
        Some(&raw_output)
    );
    record.validate_contract().unwrap();
}

#[test]
fn rovodev_changed_projection_replaces_the_same_native_event_identity() {
    let provider_session_id = "session".to_owned();
    let source_key = rovodev_source_key(&provider_session_id).unwrap();
    let session_id = rovodev_session_identity(&source_key, &provider_session_id).unwrap();
    let bound = RovoDevBoundDocument {
        source_key,
        provider_session_id,
        session_id,
        parent_session_id: None,
        unique_message_ids: HashSet::from(["message-1".to_owned()]),
    };
    let document = document();
    let project = |body: &str| {
        let raw = serde_json::json!({
            "id": "message-1",
            "timestamp": "2026-08-09T12:00:00Z",
            "role": "assistant",
            "content": body,
        });
        let event = project_message(&raw, 0, &document).unwrap().unwrap();
        core_record(&bound, [9; 32], &document, &raw, 0, event).unwrap()
    };
    let initial = project("initial exact body");
    let replacement = project("replacement exact body");
    assert_eq!(initial.event_id, replacement.event_id);
    assert_eq!(initial.session_id, replacement.session_id);
    assert_eq!(initial.native_event_id, replacement.native_event_id);
    assert_eq!(replacement.parser_revision, PARSER_REVISION);
    assert_eq!(
        replacement.content.meaningful_text(),
        "replacement exact body"
    );
    assert_ne!(
        ctx_history_core::core_record_leaf_sha256(&initial).unwrap(),
        ctx_history_core::core_record_leaf_sha256(&replacement).unwrap()
    );
}
