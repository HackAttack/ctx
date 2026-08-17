use super::*;

#[test]
fn exact_resolved_family_emits_delegated_scope() {
    let source = source_key("child").unwrap();
    let session_id = session_identity(&source, "child").unwrap();
    let session = SessionState::new(
        Path::new("/tmp/agents/a/sessions/child.jsonl"),
        "child",
        &serde_json::json!({}),
        &OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id: "parent".to_owned(),
            root_native_session_id: "parent".to_owned(),
        },
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();

    assert_eq!(
        session.relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(session.agent_scope, Some(AgentScope::Subagent));
    assert!(session.parent_session_id.is_some());
    assert!(session.root_session_id.is_some());
}

#[test]
fn contradictory_family_omits_relationship_instead_of_fallback_kind() {
    let source = source_key("child").unwrap();
    let session_id = session_identity(&source, "child").unwrap();
    let session = SessionState::new(
        Path::new("/tmp/agents/a/sessions/child.jsonl"),
        "child",
        &serde_json::json!({"parentSessionId": "other"}),
        &OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id: "parent".to_owned(),
            root_native_session_id: "parent".to_owned(),
        },
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();

    assert_eq!(session.relationship, None);
    assert_eq!(session.agent_scope, None);
    assert_eq!(session.parent_session_id, None);
    assert_eq!(session.root_session_id, None);
}

#[test]
fn absent_lineage_family_establishes_primary_scope() {
    let source = source_key("root").unwrap();
    let session_id = session_identity(&source, "root").unwrap();
    let session = SessionState::new(
        Path::new("/tmp/agents/a/sessions/root.jsonl"),
        "root",
        &serde_json::json!({}),
        &OpenClawNativeSessionFamily::Absent,
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();

    assert_eq!(session.relationship, None);
    assert_eq!(session.agent_scope, Some(AgentScope::Primary));
    assert_eq!(session.parent_session_id, None);
    assert_eq!(session.root_session_id, None);
}

#[test]
fn native_call_facts_preserve_alias_order_and_duplicates() {
    let value = serde_json::json!({
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": "call-1",
                "name": "read_file",
                "arguments": {"path": "A/../B", "file_path": "A/../B"}
            }]
        }
    });
    let calls = native_tool_calls(&value);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].file_references,
        vec!["A/../B".to_owned(), "A/../B".to_owned()]
    );
}
