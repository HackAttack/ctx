use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture, TypedKey};
use serde_json::json;

use super::copilot::copilot_activity;

#[test]
fn invocation_preserves_exact_native_identity_and_arguments() {
    let activity = copilot_activity(
        br#"{"type":"tool.execution_start","data":{"toolCallId":"call-01","mcpServerName":"source-server","mcpToolName":"source-tool","arguments":{"path":"A/../B","items":[1,1]}}}"#,
    )
    .expect("source-authoritative invocation");

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-01".to_owned()).unwrap())
    );
    let invocation = activity.invocation.expect("invocation capture");
    assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
    assert_eq!(invocation.server.as_deref(), Some("source-server"));
    assert_eq!(invocation.tool, "source-tool");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: json!({"path": "A/../B", "items": [1, 1]})
        }
    );
    assert!(activity.result.is_none());
}

#[test]
fn completion_preserves_literal_result_without_inferred_status() {
    let activity = copilot_activity(
        br#"{"type":"tool.execution_complete","data":{"toolCallId":"call-02","success":false,"error":{"message":"native failure text","code":"E_NATIVE"}}}"#,
    )
    .expect("source-authoritative completion");

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-02".to_owned()).unwrap())
    );
    assert!(activity.invocation.is_none());
    let result = activity.result.expect("result capture");
    assert_eq!(result.status, None);
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: "native failure text".to_owned()
        }
    );
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: json!({
                "success": false,
                "error": {"message": "native failure text", "code": "E_NATIVE"}
            })
        }
    );
}

#[test]
fn absent_and_ambiguous_capture_states_are_explicit() {
    let absent = copilot_activity(
        br#"{"type":"tool.execution_start","data":{"toolCallId":"call-03","mcpServerName":"server","mcpToolName":"tool"}}"#,
    )
    .expect("invocation without arguments");
    assert_eq!(
        absent.invocation.unwrap().arguments,
        ActivityJsonCapture::Absent
    );

    let duplicate = br#"{"type":"tool.execution_start","data":{"toolCallId":"first","toolCallId":"second","mcpServerName":"server","mcpToolName":"tool"}}"#;
    assert!(copilot_activity(duplicate).is_none());
}

#[test]
fn provider_event_order_and_duplicate_literal_values_are_not_rewritten() {
    let first = copilot_activity(
        br#"{"type":"tool.execution_complete","data":{"toolCallId":"same","content":"one"}}"#,
    )
    .unwrap();
    let second = copilot_activity(
        br#"{"type":"tool.execution_complete","data":{"toolCallId":"same","content":"one"}}"#,
    )
    .unwrap();

    assert_eq!(first, second);
    assert_eq!(first.result.unwrap().status, None);
}
