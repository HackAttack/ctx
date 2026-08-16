use chrono::{DateTime, Utc};
use ctx_history_core::ProviderNativeSessionRelationship;

use super::{pending_call_origin, valid_local_turn_boundary};
use crate::provider::codex::nativepath::{
    checkpoint::CodexPendingCallOriginV0, rows::CodexSessionRow,
};

fn forked_owner() -> CodexSessionRow {
    CodexSessionRow {
        native_session_id: "019fb100-0000-7000-8000-000000000002".to_owned(),
        parent_native_session_id: Some("019fb100-0000-7000-8000-000000000001".to_owned()),
        root_native_session_id: None,
        session_relationship: Some(ProviderNativeSessionRelationship::Forked),
        started_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        cwd: None,
        originator: None,
        cli_version: None,
        source_kind: None,
        external_agent_id: None,
        role_hint: None,
        model_provider: None,
        git: None,
    }
}

#[test]
fn uuid_v7_boundary_uses_only_strictly_later_embedded_timestamps() {
    let session = "019fb000-0000-7000-8000-0000000000ff";
    let later = "019fb000-0001-7000-8000-000000000000";
    let same_time_higher_randomness = "019fb000-0000-7fff-bfff-ffffffffffff";

    assert!(valid_local_turn_boundary(session, later));
    assert!(!valid_local_turn_boundary(
        session,
        same_time_higher_randomness
    ));
    assert!(!valid_local_turn_boundary(
        session,
        "550e8400-e29b-41d4-a716-446655440000"
    ));
    assert!(!valid_local_turn_boundary(
        "550e8400-e29b-41d4-a716-446655440000",
        later
    ));
}

#[test]
fn cold_forked_call_before_local_turn_is_copied_from_exact_parent() {
    let owner = forked_owner();
    assert_eq!(
        pending_call_origin(&owner, false),
        CodexPendingCallOriginV0::CopiedFromAncestor {
            ancestor_native_session_id: owner.parent_native_session_id.unwrap(),
        }
    );
}

#[test]
fn post_local_turn_forked_call_is_not_a_copy_near_miss() {
    assert_eq!(
        pending_call_origin(&forked_owner(), true),
        CodexPendingCallOriginV0::CurrentSession
    );
}
