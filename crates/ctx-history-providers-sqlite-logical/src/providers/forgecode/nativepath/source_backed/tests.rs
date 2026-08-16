#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("../source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("FORGECODE_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let lexical_text = retained"));
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
fn supported_conversations_remain_root_unknown_with_exact_native_content() {
    let selection = super::ForgeCodeSourceSelectionV0::selected(
        std::path::Path::new("/tmp/forgecode-test-data"),
        "/tmp/forgecode-test-data/.forge.db",
    );
    let source = super::ForgeCodeSourceBackedSourceV0 {
        source: selection.source_key().unwrap(),
        canonical_path: "/tmp/forgecode-test-data/.forge.db".into(),
    };
    let row = super::ForgeCodeConversationRow {
        rowid: 1,
        source_record_digest: [1; 32],
        canonical_record_bytes: 128,
        conversation_id: "forgecode-root".to_owned(),
        title: Some("ForgeCode root fixture".to_owned()),
        workspace_id: 7,
        created_at: "2026-08-05T12:00:00Z".to_owned(),
        updated_at: Some("2026-08-05T12:00:01Z".to_owned()),
        context_metadata: serde_json::json!({
            "metadata": {
                "path": "src/context-decoy.rs",
                "branch": "context-decoy"
            }
        }),
        metrics_metadata: None,
    };
    let retained = super::super::source::ForgeCodeRetainedEvent {
        event: super::super::super::event::forgecode_event(
            "forgecode-root",
            &serde_json::json!({
                "message": {
                    "text": {
                        "role": "user",
                        "content": "exact persisted ForgeCode event",
                        "metadata": {
                            "path": "src/body-decoy.rs",
                            "commit": "body-decoy"
                        }
                    }
                }
            }),
            1,
            "2026-08-05T12:00:01Z".parse().unwrap(),
        ),
        provider_event_index: 1,
    };
    let record = super::core_record(&source, &row, retained).unwrap();

    assert_eq!(record.session_relationship, None);
    assert_eq!(record.root_session_id, None);
    assert_eq!(
        record.content.meaningful_text(),
        "exact persisted ForgeCode event"
    );
    assert!(record.native_event_id.is_some());
    let facts = &record.content.activity.as_ref().unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, ctx_history_core::LiteralFactKind::Workspace);
    assert_eq!(facts[0].value, "7");
}
