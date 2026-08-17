use chrono::DateTime;
use ctx_history_core::{AgentScope, EventRole, EventType, ProviderNativeSessionRelationship};

use super::*;

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("AUGGIE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("then_some(parsed.text)"));
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
    assert_eq!(
        AUGGIE_PARSER_REVISION,
        "auggie-nativepath-json-v4-agent-scope"
    );
}

fn scope_record(parent: Option<&str>, root: Option<&str>) -> CoreRecord {
    let provider_session_id = "auggie-scope-session";
    let source = auggie_source_key(provider_session_id).unwrap();
    let session_id = auggie_session_id(&source, provider_session_id).unwrap();
    let session = ParsedAuggieSession {
        provider_session_id: provider_session_id.to_owned(),
        parent_provider_session_id: parent.map(str::to_owned),
        root_provider_session_id: root.map(str::to_owned),
        cwd: None,
    };
    let event = ParsedAuggieEvent {
        provider_event_index: 0,
        provider_event_hash: "auggie-scope-event-hash".to_owned(),
        event_type: EventType::Message,
        role: EventRole::User,
        occurred_at: DateTime::UNIX_EPOCH,
        text: "Auggie scope fixture".to_owned(),
        chat_index: 0,
        message_kind: "request",
        native_event_id: Some("auggie-scope-event".to_owned()),
    };
    auggie_core_record(&source, session_id, &session, [7; 32], event).unwrap()
}

#[test]
fn absent_or_self_root_lineage_is_primary_without_edges() {
    for root in [None, Some("auggie-scope-session")] {
        let record = scope_record(None, root);
        assert_eq!(record.agent_scope, Some(AgentScope::Primary));
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }
}

#[test]
fn durable_parent_lineage_is_subagent_with_native_edges() {
    for root in [None, Some("auggie-root")] {
        let record = scope_record(Some("auggie-parent"), root);
        assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
        assert_eq!(
            record.parent_session_id,
            Some(related_auggie_session_id("auggie-parent").unwrap())
        );
        assert_eq!(
            record.root_session_id,
            root.map(|native_id| related_auggie_session_id(native_id).unwrap())
        );
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
    }
}

#[test]
fn contradictory_or_insufficient_lineage_remains_unknown_without_edges() {
    for (parent, root) in [
        (None, Some("foreign-root")),
        (Some("auggie-scope-session"), None),
        (Some("auggie-parent"), Some("auggie-scope-session")),
    ] {
        let record = scope_record(parent, root);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }
}
