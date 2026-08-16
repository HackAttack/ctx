use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;

use super::*;

#[test]
fn protocol_classification_maps_to_content_free_product_facts() {
    for (message, expected) in [
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"search"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Product(
                ObservedMcpProductOperation::Search,
            )),
        ),
        (
            json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"private-tool-never-retained"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Unknown),
        ),
        (
            json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"blame"}}),
            McpRequestObservation::ToolCall(McpObservedTool::Unknown),
        ),
    ] {
        assert_eq!(
            request_observation(RequestDescriptor::from_message(&message)),
            expected
        );
    }
}

#[test]
fn telemetry_is_opt_in_and_never_retains_raw_error_or_cursor_values() {
    let delivered = Arc::new(Mutex::new(0));
    let disabled_delivered = delivered.clone();
    let mut disabled = McpTelemetry::start(false, move |_| {
        *disabled_delivered.lock().unwrap() += 1;
        Ok(())
    });
    disabled.record_delivered(
        RequestDescriptor::InvalidJson,
        Some(&json!({
            "error": {"code": -32700, "data": {"query": "private"}}
        })),
        Duration::ZERO,
    );
    disabled.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    assert_eq!(*delivered.lock().unwrap(), 0);

    let events = Arc::new(Mutex::new(String::new()));
    let recorded = events.clone();
    let mut telemetry = McpTelemetry::start(true, move |batch| {
        recorded.lock().unwrap().push_str(&format!("{batch:?}"));
        Ok(())
    });
    telemetry.record_delivered(
        RequestDescriptor::ToolCall {
            operation: McpToolKind::QueryEvents,
        },
        Some(&json!({"result": {"structuredContent": {
            "events": [{}, {}], "truncated": true, "next_cursor": "never-retained"
        }}})),
        Duration::ZERO,
    );
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    let serialized = events.lock().unwrap();
    assert!(!serialized.contains("never-retained"));
    assert!(!serialized.contains("private"));
}
