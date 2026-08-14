use super::*;

#[test]
fn copied_origin_targets_the_exact_duplicate_provider_occurrence() {
    let ancestor = "019fb100-0000-7000-8000-000000000001";
    let provider_identity = CodexProviderEventIdentityV0 {
        kind: CodexProviderEventIdentityKindV0::CallId,
        value: "duplicate-provider-call".to_owned(),
    };
    let first = copied_result_event_origin(
        ancestor,
        "duplicate-provider-call",
        &provider_identity,
        "tool_output",
        Some("tool"),
        0,
    )
    .unwrap()
    .unwrap();
    let second = copied_result_event_origin(
        ancestor,
        "duplicate-provider-call",
        &provider_identity,
        "tool_output",
        Some("tool"),
        1,
    )
    .unwrap()
    .unwrap();
    let event_id = |origin: ctx_history_core::EventOrigin| match origin {
        ctx_history_core::EventOrigin::CopiedFromAncestor {
            ancestor_event_id, ..
        } => *ancestor_event_id,
        origin => panic!("unexpected copied origin {origin:?}"),
    };
    assert_ne!(event_id(first), event_id(second));
}

#[test]
fn copied_origin_abstains_without_an_exact_call_identity() {
    let provider_identity = CodexProviderEventIdentityV0 {
        kind: CodexProviderEventIdentityKindV0::Id,
        value: "duplicate-provider-call".to_owned(),
    };
    assert!(copied_result_event_origin(
        "019fb100-0000-7000-8000-000000000002",
        "duplicate-provider-call",
        &provider_identity,
        "tool_output",
        Some("tool"),
        0,
    )
    .unwrap()
    .is_none());
}
