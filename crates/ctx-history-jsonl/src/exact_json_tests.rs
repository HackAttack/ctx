use crate::{exact_bounded_string_alias, raw_object_keys_are_unique, ExactJsonStringAlias};

#[test]
fn bounded_string_alias_selector_is_exact_and_fail_closed() {
    const ALIASES: &[&str] = &["callId", "call_id"];
    let cases = [
        (serde_json::json!({}), ExactJsonStringAlias::Missing),
        (
            serde_json::json!({"callId": "exact"}),
            ExactJsonStringAlias::Exact("exact"),
        ),
        (
            serde_json::json!({"call_id": "snake"}),
            ExactJsonStringAlias::Exact("snake"),
        ),
        (
            serde_json::json!({"callId": "same", "call_id": "same"}),
            ExactJsonStringAlias::Ambiguous,
        ),
        (
            serde_json::json!({"callId": "left", "call_id": "right"}),
            ExactJsonStringAlias::Ambiguous,
        ),
        (
            serde_json::json!({"callId": 7}),
            ExactJsonStringAlias::Ambiguous,
        ),
        (
            serde_json::json!({"callId": ""}),
            ExactJsonStringAlias::Ambiguous,
        ),
        (
            serde_json::json!({"callId": "oversized"}),
            ExactJsonStringAlias::Ambiguous,
        ),
    ];
    for (object, expected) in &cases {
        assert_eq!(
            exact_bounded_string_alias(object.as_object().unwrap(), ALIASES, 8),
            *expected
        );
    }
    assert!(!raw_object_keys_are_unique(
        br#"{"callId":"first","callId":"second"}"#
    ));
}
