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
        "payload_type": "session_transcript",
        "ctx_session_id": "018f45d0-0000-7000-8000-000000000001",
        "mode": "log",
        "events": [{
            "ctx_event_id": event_id,
            "text": expanded,
        }],
        "pagination": {
            "limit": 1,
            "returned": 1,
            "has_more": true,
            "next_cursor": "opaque-next-page"
        },
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
fn query_events_detection_and_final_response_bound_are_exact() {
    let call = json!({
        "method": "tools/call",
        "params": {"name": "query_events", "arguments": {}},
    });
    assert!(is_query_events_tool_call(&call));
    assert!(!is_query_events_tool_call(&json!({
        "method": "tools/call",
        "params": {"name": "show_event", "arguments": {}},
    })));

    let response_id = json!(9);
    let response = success_response(
        response_id.clone(),
        tool_result(json!({
            "payload_type": "event_range_page",
            "events": [{"text": "x".repeat(2_000)}],
            "next_cursor": "opaque-cursor"
        })),
    );
    let exact = serialized_json_line_bytes(&response).unwrap();
    assert_eq!(
        bound_query_events_mcp_response(response.clone(), response_id.clone(), exact),
        response
    );

    let bounded = bound_query_events_mcp_response(response, response_id, TEST_OUTPUT_LIMIT);
    assert_eq!(bounded["result"]["isError"], true);
    assert_eq!(
        bounded["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert!(bounded["result"]["structuredContent"]
        .get("next_cursor")
        .is_none());
    assert!(serialized_json_line_bytes(&bounded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn final_mcp_serialization_is_bounded_after_json_expansion() {
    assert_eq!(
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
        8 * 1024 * 1024
    );
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
    assert!(decoded["result"]["structuredContent"]
        .get("pagination")
        .is_none());
    assert!(!decoded["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("opaque-next-page"));
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
