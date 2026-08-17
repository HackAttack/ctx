use super::*;

fn timestamp_event(timestamp: &str) -> GooseNativeEvent {
    GooseNativeEvent {
        sqlite_rowid: 41,
        native_order: 41,
        native_identity: "goose-message-identity-v1:timestamp-parity".to_owned(),
        provider_message_identity: Some("timestamp-parity".to_owned()),
        identity_degraded: false,
        session_identity: "timestamp-session".to_owned(),
        kind: GooseNativeEventKind::Message,
        role: "assistant".to_owned(),
        content: serde_json::json!([{"type": "text", "text": "timestamp parity"}]),
        searchable_text: "timestamp parity".to_owned(),
        semantic_capture_ambiguous: false,
        created_timestamp: None,
        timestamp: Some(timestamp.to_owned()),
        tokens_json: None,
        metadata_json: None,
        retained_content_bytes: 16,
        logical_row_digest: Some([7; 32]),
    }
}

#[test]
fn goose_timestamp_forms_preserve_provider_time_order_and_identity() {
    let source = goose_source_key().unwrap();
    let session_id = goose_session_id(&source, "timestamp-session").unwrap();
    let session = GooseSessionProjection {
        session_id,
        parent_session_id: None,
        parent_provider_session_id: None,
        cwd: None,
    };
    let project =
        |timestamp| goose_core_record(&source, &session, timestamp_event(timestamp), 9).unwrap();

    let rfc3339 = project("2026-06-24T12:00:00.123Z");
    let naive = project("2026-06-24 12:00:00.123");
    let numeric_seconds = project("1782302400.123");
    let numeric_millis = project("1782302400123");
    let native_event_id = TypedKey::utf8("timestamp-parity").unwrap();

    for projected in [&rfc3339, &naive, &numeric_seconds, &numeric_millis] {
        assert_eq!(projected.agent_scope, Some(AgentScope::Primary));
        assert_eq!(projected.occurred_at_unix_ms, Some(1_782_302_400_123));
        assert_eq!(projected.event_sequence, 9);
        assert_eq!(projected.event_id, rfc3339.event_id);
        assert_eq!(projected.native_event_id.as_ref(), Some(&native_event_id));
    }
}

#[test]
fn native_parent_session_classifies_subagent_scope() {
    let source = goose_source_key().unwrap();
    let session_id = goose_session_id(&source, "timestamp-session").unwrap();
    let parent_session_id = goose_session_id(&source, "parent-session").unwrap();
    let session = GooseSessionProjection {
        session_id,
        parent_session_id: Some(parent_session_id),
        parent_provider_session_id: Some("parent-session".to_owned()),
        cwd: None,
    };

    let record = goose_core_record(&source, &session, timestamp_event("1782302400"), 9).unwrap();
    assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(record.parent_session_id, Some(parent_session_id));
}

#[test]
fn goose_changed_projection_replaces_the_same_native_event_identity() {
    let source = goose_source_key().unwrap();
    let session_id = goose_session_id(&source, "timestamp-session").unwrap();
    let session = GooseSessionProjection {
        session_id,
        parent_session_id: None,
        parent_provider_session_id: None,
        cwd: None,
    };
    let project = |body: &str| {
        let mut event = timestamp_event("2026-08-09T12:00:00Z");
        event.searchable_text = body.to_owned();
        event.content = serde_json::json!([{"type": "text", "text": body}]);
        goose_core_record(&source, &session, event, 9).unwrap()
    };
    let initial = project("initial exact Goose body");
    let replacement = project("replacement exact Goose body");
    assert_eq!(initial.event_id, replacement.event_id);
    assert_eq!(initial.session_id, replacement.session_id);
    assert_eq!(initial.native_event_id, replacement.native_event_id);
    assert_eq!(replacement.parser_revision, GOOSE_PARSER_REVISION);
    assert_eq!(
        replacement.content.meaningful_text(),
        "replacement exact Goose body"
    );
    assert_ne!(
        ctx_history_core::core_record_leaf_sha256(&initial).unwrap(),
        ctx_history_core::core_record_leaf_sha256(&replacement).unwrap()
    );
}

#[test]
fn nested_goose_metadata_keys_never_escape_into_facts() {
    let source = goose_source_key().unwrap();
    let session_id = goose_session_id(&source, "closed-facts-session").unwrap();
    let session = GooseSessionProjection {
        session_id,
        parent_session_id: None,
        parent_provider_session_id: None,
        cwd: Some("/schema-known-cwd".to_owned()),
    };
    let mut event = timestamp_event("2026-08-16T00:00:00Z");
    event.session_identity = "closed-facts-session".to_owned();
    event.content = serde_json::json!([{
        "type": "text",
        "text": "exact Goose body",
        "metadata": {
            "path": "src/goose-decoy.rs",
            "nested": {
                "branch": "decoy-branch",
                "commit": "decoy-commit",
                "command": "decoy-command"
            }
        }
    }]);
    event.searchable_text = "exact Goose body".to_owned();

    let record = goose_core_record(&source, &session, event, 11).unwrap();
    let facts = &record.content.activity.as_ref().unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, LiteralFactKind::SessionCwd);
    assert_eq!(facts[0].value, "/schema-known-cwd");
}

#[test]
fn oversized_goose_result_is_retained_once_with_explicit_activity_omission() {
    let source = goose_source_key().unwrap();
    let session_id = goose_session_id(&source, "large-session").unwrap();
    let session = GooseSessionProjection {
        session_id,
        parent_session_id: None,
        parent_provider_session_id: None,
        cwd: None,
    };
    let body = format!(
        "goose-whole-result-head-{}-goose-whole-result-tail",
        "x".repeat(9 * 1024 * 1024)
    );
    let event = GooseNativeEvent {
        sqlite_rowid: 42,
        native_order: 42,
        native_identity: "goose-message-identity-v1:large-result".to_owned(),
        provider_message_identity: Some("large-result".to_owned()),
        identity_degraded: false,
        session_identity: "large-session".to_owned(),
        kind: GooseNativeEventKind::ToolOutput,
        role: "tool".to_owned(),
        content: serde_json::json!([{
            "type": "toolResponse",
            "toolCallId": "large-call",
            "toolResult": body.clone(),
        }]),
        searchable_text: body.clone(),
        semantic_capture_ambiguous: false,
        created_timestamp: None,
        timestamp: None,
        tokens_json: None,
        metadata_json: None,
        retained_content_bytes: u64::try_from(body.len()).unwrap(),
        logical_row_digest: Some([8; 32]),
    };
    let record = goose_core_record(&source, &session, event, 10).unwrap();
    assert_eq!(record.content.meaningful_text(), body);
    assert!(record.content.structured_content.is_none());
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("large-call".to_owned()))
    );
    let result = activity.result.as_ref().unwrap();
    assert_eq!(result.text, ActivityTextCapture::NormalizedBody);
    assert!(matches!(
        result.structured_content,
        ActivityJsonCapture::Omitted { ref reason, .. }
            if reason == "normalized_body_authoritative"
    ));
    record.validate_contract().unwrap();
}
