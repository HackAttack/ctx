use std::{fs, time::Duration};

use ctx_client_observability::mcp_observation::{McpObservedTool, McpRequestObservation};
use serde_json::json;

use super::*;

#[test]
fn protocol_classification_maps_to_content_free_product_facts() {
    for (message, expected_descriptor, expected_observation) in [
        (
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "search"}
            }),
            RequestDescriptor::ToolCall {
                operation: McpToolKind::Search,
            },
            McpRequestObservation::ToolCall(McpObservedTool::Product(
                crate::operation_descriptor::ObservedMcpProductOperation::Search,
            )),
        ),
        (
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "private-tool-never-retained"}
            }),
            RequestDescriptor::ToolCall {
                operation: McpToolKind::Unknown,
            },
            McpRequestObservation::ToolCall(McpObservedTool::Unknown),
        ),
        (
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {}}),
            RequestDescriptor::ToolCall {
                operation: McpToolKind::Missing,
            },
            McpRequestObservation::ToolCall(McpObservedTool::Missing),
        ),
        (
            json!({"jsonrpc": "2.0", "id": 4, "method": "private-method"}),
            RequestDescriptor::UnknownRequest,
            McpRequestObservation::UnknownRequest,
        ),
    ] {
        let descriptor = RequestDescriptor::from_message(&message);
        assert_eq!(descriptor, expected_descriptor);
        assert_eq!(request_observation(descriptor), expected_observation);
    }
}

#[test]
fn query_events_uses_only_bounded_page_metadata() {
    let response = json!({
        "result": {
            "structuredContent": {
                "payload_type": "event_range_page",
                "events": [{}, {}],
                "truncated": true,
                "next_cursor": "opaque-and-never-recorded"
            }
        }
    });

    let metadata = result_metadata(McpToolKind::QueryEvents, &response);

    assert_eq!(
        metadata.result_count,
        Some(crate::analytics::count_bucket(2))
    );
    assert_eq!(metadata.zero_result, Some(false));
    assert_eq!(metadata.result_truncated, Some(true));
    assert_eq!(metadata.events_truncated, None);
}

#[test]
fn raw_error_text_and_pro_payload_dimensions_are_not_observed() {
    let error = json!({
        "code": -32602,
        "message": "private parser detail /home/person/secret",
        "data": {"query": "do not retain"}
    });
    assert_eq!(
        json_rpc_error_class(RequestDescriptor::UnknownRequest, &error),
        McpErrorClassV1::InvalidParams
    );

    let response = json!({
        "result": {
            "structuredContent": {
                "matches": [1, 2, 3],
                "results": [1, 2, 3],
                "cursor": "private-and-never-recorded"
            }
        }
    });
    for operation in [McpToolKind::Blame, McpToolKind::ProStatus] {
        assert_eq!(
            result_metadata(operation, &response),
            McpResultMetadataV1::default()
        );
    }
}

#[test]
fn startup_and_dynamic_opt_out_precede_identity_marker_and_delivery() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let output = root.path().join("telemetry.jsonl");
    let config = root.path().join("config.toml");

    fs::write(&config, "[analytics]\nenabled = false\n").unwrap();
    let disabled = McpTelemetry::start(root.path().to_path_buf());
    assert!(disabled.observation.is_none());
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [std::ffi::OsString::from("config.toml")]
    );

    fs::write(
        &config,
        format!(
            "[analytics]\nenabled = true\nendpoint = {:?}\n",
            format!("file://{}", output.display())
        ),
    )
    .unwrap();
    let mut telemetry = McpTelemetry::start(root.path().to_path_buf());
    assert!(telemetry.observation.is_some());

    fs::write(&config, "[analytics]\nenabled = false\n").unwrap();
    telemetry.record_delivered(
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Status,
        },
        Some(&json!({"result": {"structuredContent": {}}})),
        Duration::ZERO,
    );
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);

    assert!(!output.exists());
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [std::ffi::OsString::from("config.toml")]
    );
}
