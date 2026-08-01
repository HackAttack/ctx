use serde_json::{json, Value};

use super::*;
use crate::mcp::response::{success_response, tool_result};

const TEST_OUTPUT_LIMIT: usize = 1_024;

fn expanded_response() -> (Value, Value, Uuid) {
    let event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap();
    let response_id = json!("request-\"\\\n\u{0001}-雪");
    let expanded = "\"\\\n\r\t\u{0000}\u{001f}雪".repeat(200);
    let structured = json!({
        "schema_version": 1,
        "payload_type": "event_window",
        "ctx_event_id": event_id,
        "event": {
            "ctx_event_id": event_id,
            "text": expanded,
        },
        "events": [{"ctx_event_id": event_id}],
    });
    let response = success_response(response_id.clone(), tool_result(structured));
    (response, response_id, event_id)
}

#[test]
fn show_tool_call_detection_covers_both_show_tools() {
    let event = json!({
        "method": "tools/call",
        "params": {"name": "show_event", "arguments": {}},
    });
    assert!(is_show_tool_call(&event));

    assert!(is_show_tool_call(&json!({
        "method": "tools/call",
        "params": {"name": "show_session", "arguments": {}},
    })));

    for message in [
        json!({"method": "tools/call", "params": {"name": "search", "arguments": {}}}),
        json!({"method": "ping"}),
    ] {
        assert!(!is_show_tool_call(&message));
    }
}

#[test]
fn final_mcp_serialization_is_bounded_after_json_expansion() {
    let (response, response_id, event_id) = expanded_response();
    let serialized_bytes = serialized_json_line_bytes(&response).unwrap();
    assert!(serialized_bytes > TEST_OUTPUT_LIMIT);

    let bounded = bound_show_mcp_response(response, response_id.clone(), TEST_OUTPUT_LIMIT);
    let encoded = serde_json::to_string(&bounded).unwrap();
    let decoded: Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded["id"], response_id);
    assert_eq!(decoded["result"]["isError"], true);
    assert_eq!(
        decoded["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert_eq!(
        decoded["result"]["structuredContent"]["ctx_event_id"],
        event_id.to_string()
    );
    assert!(serialized_json_line_bytes(&decoded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn final_mcp_serialization_preserves_exact_response_at_the_limit() {
    let (response, response_id, _) = expanded_response();
    let exact_limit = serialized_json_line_bytes(&response).unwrap();
    let bounded = bound_show_mcp_response(response.clone(), response_id, exact_limit);

    assert_eq!(bounded, response);
    assert_eq!(serialized_json_line_bytes(&bounded).unwrap(), exact_limit);
}
