use serde_json::{json, Value};

use super::{compact_json, render_tool_text};
use crate::tool_backend::{CursorFailureKind, ToolBackendError};

pub(super) fn invalid_tool_request(message: impl Into<String>) -> ToolBackendError {
    ToolBackendError::invalid_request(message)
}

pub(super) fn tool_result(structured: Value) -> Value {
    let text = render_tool_text(&structured);
    tool_result_with_text(structured, text)
}

pub(super) fn tool_result_with_text(structured: Value, text: String) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

fn structured_error_result(structured: Value) -> Value {
    let text = render_diagnostic_text(&structured);
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

fn render_diagnostic_text(structured: &Value) -> String {
    let message = structured
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| structured.get("error").and_then(Value::as_str))
        .unwrap_or("ctx blame failed");
    let mut text = single_line(message);
    if let Some(argv) = structured
        .pointer("/next_action/argv")
        .and_then(Value::as_array)
        .filter(|argv| !argv.is_empty())
    {
        text.push_str("\nNext:");
        for argument in argv.iter().filter_map(Value::as_str) {
            text.push(' ');
            text.push_str(&display_argument(argument));
        }
    }
    text
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn display_argument(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        argument.to_owned()
    } else {
        serde_json::to_string(argument).unwrap_or_else(|_| "\"<invalid argument>\"".to_owned())
    }
}

pub(super) fn tool_error_result(error: ToolBackendError) -> Value {
    let error = match error {
        ToolBackendError::Pro { structured, .. } => return structured_error_result(structured),
        error => error,
    };
    let text = error.to_string();
    let structured = match error {
        ToolBackendError::InvalidRequest { detail } => json!({
            "error": detail,
            "error_code": "invalid_request",
        }),
        ToolBackendError::EventQuery(error) => {
            crate::commands::list::events::event_query_error_value(&error)
        }
        ToolBackendError::GenerationAuthority(error) => {
            crate::commands::source_index::generation_query_authority_error_json(&error)
        }
        ToolBackendError::GenerationChanged => json!({
            "error": "generation_changed/active_generation_race",
            "error_code": "generation_changed",
            "failure_kind": "active_generation_race",
            "detail": "the active searchable generation changed while the command was opening it",
            "retryable": true,
        }),
        ToolBackendError::Cursor { kind, detail } => json!({
            "error": detail.clone(),
            "error_code": match kind {
                CursorFailureKind::Stale => "cursor_stale",
                CursorFailureKind::Mismatch => "cursor_mismatch",
                CursorFailureKind::Invalid => "invalid_cursor",
            },
            "detail": detail,
            "retryable": false,
        }),
        ToolBackendError::OutputLimit {
            event_id,
            actual_bytes,
            maximum_bytes,
        } => json!({
            "error": "output_limit_exceeded",
            "error_code": "output_limit_exceeded",
            "ctx_event_id": event_id,
            "actual_bytes": actual_bytes,
            "maximum_bytes": maximum_bytes,
            "retryable": false,
            "remediation": "reduce the event window or choose a narrower transcript mode",
        }),
        ToolBackendError::SemanticNotReady {
            code,
            detail,
            retryable,
        } => json!({
            "error": text.clone(),
            "error_code": code,
            "detail": detail,
            "retryable": retryable,
        }),
        ToolBackendError::Internal { detail } => json!({ "error": detail }),
        ToolBackendError::Pro { .. } => unreachable!("Pro failures return above"),
    };
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

pub(super) fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub(super) fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let error = compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub(super) fn invalid_request_response(id: Option<&Value>) -> Value {
    let id = match id {
        Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
        None | Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => {
            Value::Null
        }
    };
    error_response(id, -32600, "Invalid Request", None)
}

pub(super) fn json_rpc_error(code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{
        error_response, invalid_request_response, invalid_tool_request, structured_error_result,
        tool_error_result, tool_result,
    };
    use crate::tool_backend::{CursorFailureKind, ToolBackendError};

    #[test]
    fn generation_change_is_a_retryable_typed_tool_error() {
        let result = tool_error_result(ToolBackendError::GenerationChanged);

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"],
            "generation_changed/active_generation_race"
        );
        assert_eq!(
            result["structuredContent"]["error_code"],
            "generation_changed"
        );
        assert_eq!(
            result["structuredContent"]["failure_kind"],
            "active_generation_race"
        );
        assert_eq!(result["structuredContent"]["retryable"], true);
    }

    #[test]
    fn error_response_preserves_required_null_id_while_pruning_optional_data() {
        let response = error_response(serde_json::Value::Null, -32700, "Parse error", None);

        assert!(response.as_object().unwrap().contains_key("id"));
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["code"], -32700);
        assert!(!response["error"].as_object().unwrap().contains_key("data"));
    }

    #[test]
    fn error_response_preserves_string_and_numeric_ids_exactly() {
        let string_id = invalid_request_response(Some(&json!("request-7")));
        let numeric_id = invalid_request_response(Some(&json!(7)));

        assert_eq!(string_id["id"], "request-7");
        assert_eq!(numeric_id["id"], 7);
    }

    #[test]
    fn invalid_request_response_uses_null_for_unknown_or_invalid_ids() {
        let unknown = invalid_request_response(None);
        assert!(unknown.as_object().unwrap().contains_key("id"));
        assert!(unknown["id"].is_null());

        for id in [json!(null), json!(true), json!([])] {
            let response = invalid_request_response(Some(&id));
            assert!(response.as_object().unwrap().contains_key("id"));
            assert!(response["id"].is_null());
        }
    }

    #[test]
    fn invalid_tool_request_preserves_detail_and_adds_stable_error_code() {
        let result = tool_error_result(invalid_tool_request("limit must be an integer"));

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"],
            "limit must be an integer"
        );
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert_eq!(result["content"][0]["text"], "limit must be an integer");
    }

    #[test]
    fn pro_error_structured_content_is_the_cli_json_diagnostic() {
        let error = anyhow::anyhow!("resource_not_found");
        let mut cli_json = Vec::new();
        assert!(crate::pro::write_stable_error_json(&mut cli_json, &error).unwrap());
        let expected: serde_json::Value = serde_json::from_slice(&cli_json).unwrap();

        let result = tool_error_result(ToolBackendError::Pro {
            code: "resource_not_found",
            diagnostic: "resource_not_found".to_owned(),
            structured: expected.clone(),
        });
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"], expected);
        let expected_text = result["structuredContent"]
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| result["structuredContent"]["error"].as_str())
            .unwrap();
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(expected_text)));
    }

    #[test]
    fn diagnostic_text_is_rendered_from_message_and_one_typed_argv_action() {
        let structured = json!({
            "error": "resource_not_found",
            "error_code": "resource_not_found",
            "reason": "target_not_indexed",
            "message": "The current Pro graph does not contain this blame target.",
            "retryable": false,
            "next_action": {
                "kind": "search_core",
                "argv": ["ctx", "search", "src/file with spaces.rs", "--refresh", "off"]
            }
        });
        let result = structured_error_result(structured.clone());

        assert_eq!(result["structuredContent"], structured);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "The current Pro graph does not contain this blame target.\nNext: ctx search \"src/file with spaces.rs\" --refresh off"
        );
    }

    #[test]
    fn conflicting_attribution_remains_a_successful_tool_result() {
        let structured = json!({
            "target": {"kind": "commit"},
            "outcome": {
                "attribution": "conflicting",
                "coverage": {
                    "unit": "commit_fact",
                    "evaluated": 1,
                    "proven": 0,
                    "possible": 0,
                    "conflicting": 1,
                    "none": 0
                }
            },
            "freshness": {"state": "current"},
            "matches": [],
            "evidence": []
        });
        let result = tool_result(structured.clone());

        assert!(result.get("isError").is_none());
        assert_eq!(result["structuredContent"], structured);
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Producer evidence conflicts")));
    }

    #[test]
    fn presentation_output_limit_has_stable_structured_content() {
        let event_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let error = crate::presentation_limit::PresentationOutputLimitError {
            event_id,
            actual_bytes: 2048,
            maximum_bytes: 1024,
        };
        let result = tool_error_result(ToolBackendError::OutputLimit {
            event_id,
            actual_bytes: 2048,
            maximum_bytes: 1024,
        });

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"],
            crate::presentation_limit::presentation_output_limit_error_json(&error)
        );
        assert_eq!(
            result["structuredContent"]["error_code"],
            "output_limit_exceeded"
        );
        assert_eq!(result["structuredContent"]["retryable"], false);
        assert_eq!(result["content"][0]["text"], error.to_string());
    }

    #[test]
    fn session_cursor_failures_have_stable_typed_tool_errors() {
        let cases = [
            (
                ctx_history_index::IndexError::SessionEventCursorGenerationMismatch {
                    cursor_generation: "old".to_owned(),
                    pinned_generation: "new".to_owned(),
                },
                CursorFailureKind::Stale,
                "cursor_stale",
            ),
            (
                ctx_history_index::IndexError::SessionEventCursorSessionMismatch,
                CursorFailureKind::Mismatch,
                "cursor_mismatch",
            ),
            (
                ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate,
                CursorFailureKind::Invalid,
                "invalid_cursor",
            ),
        ];

        for (error, kind, code) in cases {
            let message = error.to_string();
            let result = tool_error_result(ToolBackendError::Cursor {
                kind,
                detail: message.clone(),
            });
            assert_eq!(result["isError"], true);
            assert_eq!(result["structuredContent"]["error_code"], code);
            assert_eq!(result["structuredContent"]["retryable"], false);
            assert_eq!(result["structuredContent"]["detail"], message);
            assert_eq!(result["content"][0]["text"], message);
        }
    }
}
