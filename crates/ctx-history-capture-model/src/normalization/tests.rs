use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::*;

const LEGACY_CANONICAL_TEXT_BOUND: usize = 16_000;

struct TestPolicyEvent {
    payload: Value,
}

fn test_native_event(event_type: EventType, text: &str, body: Value) -> TestPolicyEvent {
    let retained_text = provider_policy_event_text(event_type, text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    TestPolicyEvent {
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "source_format": "test_provider",
            "body": retained_body,
        }),
    }
}

#[test]
fn native_event_retains_complete_message_tool_arguments_results_and_patches() {
    let message = format!(
        "{}MESSAGE_TAIL_ORACLE",
        "message-content-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 8)
    );
    let arguments = format!(
        "{}ARGUMENT_TAIL_ORACLE",
        "structured-tool-argument-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 12)
    );
    let result = format!(
        "{}RESULT_TAIL_ORACLE",
        "successful-command-output-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 12)
    );
    let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch";

    for event in [
        test_native_event(EventType::Message, &message, json!({"content": message})),
        test_native_event(
            EventType::ToolCall,
            &arguments,
            json!({"tool_name": "Edit", "arguments": arguments, "patch": patch}),
        ),
        test_native_event(
            EventType::CommandOutput,
            &result,
            json!({"exit_code": 0, "stdout": result, "diff": patch}),
        ),
    ] {
        assert_eq!(
            event.payload["text_retention"],
            json!({
                "mode": "complete",
                "limit_chars": null,
                "truncated": false,
                "omission_policy": "none",
                "omission_applied": false,
            })
        );
        let rendered = event.payload.to_string();
        assert!(!rendered.contains("field_retention"));
        assert!(!rendered.contains("provider_truncation"));
    }

    let message_event =
        test_native_event(EventType::Message, &message, json!({"content": message}));
    assert!(message_event.payload["text"]
        .as_str()
        .unwrap()
        .ends_with("MESSAGE_TAIL_ORACLE"));
    let tool_event = test_native_event(
        EventType::ToolCall,
        &arguments,
        json!({"arguments": arguments, "patch": patch}),
    );
    assert!(tool_event.payload["body"]["arguments"]
        .as_str()
        .unwrap()
        .ends_with("ARGUMENT_TAIL_ORACLE"));
    assert_eq!(tool_event.payload["body"]["patch"], patch);
    let result_event = test_native_event(
        EventType::CommandOutput,
        &result,
        json!({"exit_code": 0, "stdout": result, "diff": patch}),
    );
    assert!(result_event.payload["body"]["stdout"]
        .as_str()
        .unwrap()
        .ends_with("RESULT_TAIL_ORACLE"));
    assert_eq!(result_event.payload["body"]["diff"], patch);
}
