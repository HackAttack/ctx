#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("CONTINUE_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.search_text.clone()"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn supported_sessions_remain_root_unknown_with_exact_native_content() {
    use super::super::normalize::{
        ContinueEventIdentity, ContinueEventKind, ContinueEventRole, ContinueEventRow,
        ContinueSessionIdentity, ContinueSessionRow,
    };

    let session_identity = ContinueSessionIdentity("continue-root".to_owned());
    let source = super::continue_source_key(&session_identity.0).unwrap();
    let session_id = super::continue_session_id(&source, &session_identity.0).unwrap();
    let session = ContinueSessionRow {
        identity: session_identity.clone(),
        title: Some("Continue root fixture".to_owned()),
        started_at: Some("2026-08-05T12:00:00Z".parse().unwrap()),
        workspace_directory: Some("/workspace/continue".to_owned()),
        mode: Some("chat".to_owned()),
        chat_model_title: Some("test-model".to_owned()),
        usage: None,
        index_metadata: None,
        metadata_json: "{}".to_owned(),
        metadata_hash: "continue-test-metadata".to_owned(),
    };
    let event = ContinueEventRow {
        identity: ContinueEventIdentity {
            session: session_identity,
            history_ordinal: 3,
        },
        native_item_id: Some("continue-event-3".to_owned()),
        kind: ContinueEventKind::Message,
        role: ContinueEventRole::User,
        occurred_at: Some("2026-08-05T12:00:01Z".parse().unwrap()),
        search_text: "exact persisted Continue event".to_owned(),
        calls: Vec::new().into_boxed_slice(),
    };
    let record = super::project_bound_event(&source, session_id, [3; 32], &session, event).unwrap();

    assert_eq!(record.session_relationship, None);
    assert_eq!(record.root_session_id, None);
    assert_eq!(
        record.content.meaningful_text(),
        "exact persisted Continue event"
    );
    assert!(record.native_event_id.is_some());
}

#[test]
fn conflicting_continue_outer_and_nested_call_ids_retain_the_event_and_abstain() {
    use ctx_history_core::ActivityJsonCapture;

    use super::super::normalize::{
        ContinueCallRelationship, ContinueEventIdentity, ContinueEventKind, ContinueEventRole,
        ContinueEventRow, ContinueSessionIdentity, ContinueSessionRow,
    };

    let session_identity = ContinueSessionIdentity("continue-aliases".to_owned());
    let source = super::continue_source_key(&session_identity.0).unwrap();
    let session_id = super::continue_session_id(&source, &session_identity.0).unwrap();
    let session = ContinueSessionRow {
        identity: session_identity.clone(),
        title: None,
        started_at: None,
        workspace_directory: None,
        mode: None,
        chat_model_title: None,
        usage: None,
        index_metadata: None,
        metadata_json: "{}".to_owned(),
        metadata_hash: "continue-alias-test".to_owned(),
    };
    let event = ContinueEventRow {
        identity: ContinueEventIdentity {
            session: session_identity,
            history_ordinal: 0,
        },
        native_item_id: Some("continue-alias-event".to_owned()),
        kind: ContinueEventKind::ToolCall,
        role: ContinueEventRole::Assistant,
        occurred_at: None,
        search_text: "retained ambiguous Continue call".to_owned(),
        calls: vec![ContinueCallRelationship {
            state_ordinal: 0,
            call_id: Some("outer-call".to_owned()),
            nested_call_id: Some("nested-call".to_owned()),
            tool_name: Some("exact_tool".to_owned()),
            status: None,
            arguments: ActivityJsonCapture::Unavailable,
        }]
        .into_boxed_slice(),
    };
    let record = super::project_bound_event(&source, session_id, [4; 32], &session, event).unwrap();
    assert_eq!(
        record.content.meaningful_text(),
        "retained ambiguous Continue call"
    );
    assert!(record.content.activity.is_none());
    assert_eq!(
        record.content.structured_content.as_ref().unwrap()["calls"][0]["call_id"],
        "outer-call"
    );
    assert_eq!(
        record.content.structured_content.as_ref().unwrap()["calls"][0]["nested_call_id"],
        "nested-call"
    );
}

#[test]
fn continue_changed_projection_replaces_the_same_native_event_identity() {
    use super::super::normalize::{
        ContinueEventIdentity, ContinueEventKind, ContinueEventRole, ContinueEventRow,
        ContinueSessionIdentity, ContinueSessionRow,
    };

    let session_identity = ContinueSessionIdentity("continue-replacement".to_owned());
    let source = super::continue_source_key(&session_identity.0).unwrap();
    let session_id = super::continue_session_id(&source, &session_identity.0).unwrap();
    let session = ContinueSessionRow {
        identity: session_identity.clone(),
        title: None,
        started_at: None,
        workspace_directory: None,
        mode: None,
        chat_model_title: None,
        usage: None,
        index_metadata: None,
        metadata_json: "{}".to_owned(),
        metadata_hash: "continue-replacement-test".to_owned(),
    };
    let project = |body: &str| {
        super::project_bound_event(
            &source,
            session_id,
            [5; 32],
            &session,
            ContinueEventRow {
                identity: ContinueEventIdentity {
                    session: session_identity.clone(),
                    history_ordinal: 4,
                },
                native_item_id: Some("continue-replacement-event".to_owned()),
                kind: ContinueEventKind::Message,
                role: ContinueEventRole::Assistant,
                occurred_at: None,
                search_text: body.to_owned(),
                calls: Vec::new().into_boxed_slice(),
            },
        )
        .unwrap()
    };
    let initial = project("initial exact Continue body");
    let replacement = project("replacement exact Continue body");
    assert_eq!(initial.event_id, replacement.event_id);
    assert_eq!(initial.session_id, replacement.session_id);
    assert_eq!(initial.native_event_id, replacement.native_event_id);
    assert_eq!(
        replacement.parser_revision,
        super::CONTINUE_SOURCE_BACKED_PARSER_REVISION
    );
    assert_eq!(
        replacement.content.meaningful_text(),
        "replacement exact Continue body"
    );
    assert_ne!(
        ctx_history_core::core_record_leaf_sha256(&initial).unwrap(),
        ctx_history_core::core_record_leaf_sha256(&replacement).unwrap()
    );
}
