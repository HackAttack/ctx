use super::*;
use serde_json::json;

#[test]
fn retains_complete_results_and_rejects_ambiguity_for_cline_and_roo() {
    let identity = ClineTaskIdentity::new("shared-task");
    let parse = |value: serde_json::Value| {
        let raw = RawValue::from_string(value.to_string()).unwrap();
        parse_item(
            &raw,
            ItemParseContext {
                identity: &identity,
                component: ClineEventComponent::ApiHistory,
                max_item_units: 60,
            },
            0,
            &mut BTreeMap::new(),
            &mut ClinePublicationStats::default(),
        )
    };
    let parse_raw = |value: &str| {
        let raw = RawValue::from_string(value.to_owned()).unwrap();
        parse_item(
            &raw,
            ItemParseContext {
                identity: &identity,
                component: ClineEventComponent::ApiHistory,
                max_item_units: 60,
            },
            0,
            &mut BTreeMap::new(),
            &mut ClinePublicationStats::default(),
        )
    };

    for status in [Some("success"), Some("failure"), None] {
        let mut value = json!({
            "role": "tool",
            "tool_use_id": "call-1",
            "content": format!("complete-{}", status.unwrap_or("absent")),
        });
        if let Some(status) = status {
            value["status"] = json!(status);
        }
        let item = parse(value);
        assert!(item.rejection.is_none());
        assert_eq!(item.rows.len(), 1);
        let expected_body = format!("complete-{}", status.unwrap_or("absent"));
        assert_eq!(item.rows[0].body.as_deref(), Some(expected_body.as_str()));
        let output = item.rows[0].sparse_output.as_ref().unwrap();
        assert_eq!(output.status.as_deref(), status);
        assert_eq!(output.call_id.as_deref(), Some("call-1"));
    }

    let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
    let item = parse(json!({
        "role": "tool",
        "tool_use_id": "large-call",
        "content": large,
        "status": "success",
    }));
    assert!(item.rejection.is_none());
    assert_eq!(
        item.rows[0].body.as_deref().unwrap().len(),
        9 * 1024 * 1024 + 4
    );
    assert!(item.rows[0].body.as_deref().unwrap().ends_with("tail"));

    let ambiguous = parse(json!({
        "role": "tool",
        "content": "first",
        "output": "second",
    }));
    assert_eq!(
        ambiguous.rejection.as_ref().map(|value| value.kind),
        Some(ClineItemRejectionKind::ConflictingDiscriminator)
    );

    let aliases = parse(json!({
        "role": "assistant",
        "content": {
            "type": "tool_use",
            "tool_use_id": "first-call",
            "call_id": "second-call",
            "name": "first_tool",
            "tool": "second_tool",
            "input": {"x": 1},
            "arguments": {"x": 2}
        }
    }));
    assert!(aliases.rejection.is_none());
    assert_eq!(aliases.rows.len(), 1);
    let call = aliases.rows[0].tool_call.as_ref().unwrap();
    assert!(call.call_id.is_none());
    assert!(call.name.is_none());
    assert_eq!(call.arguments, ActivityJsonCapture::Unavailable);
    assert_eq!(
        aliases.rows[0].structured_content["content"]["tool_use_id"],
        "first-call"
    );
    assert_eq!(
        aliases.rows[0].structured_content["content"]["call_id"],
        "second-call"
    );

    let duplicate_call_fields = parse_raw(
        r#"{"role":"assistant","content":{"type":"tool_use","tool_use_id":"call-1","name":"tool","name":"tool","arguments":{"x":1},"arguments":{"x":1}}}"#,
    );
    assert_eq!(
        duplicate_call_fields
            .rejection
            .as_ref()
            .map(|value| value.kind),
        Some(ClineItemRejectionKind::ConflictingDiscriminator)
    );

    let duplicate_status = parse_raw(
        r#"{"role":"tool","tool_use_id":"call-1","status":"success","status":"success","content":"exact body"}"#,
    );
    assert_eq!(
        duplicate_status.rejection.as_ref().map(|value| value.kind),
        Some(ClineItemRejectionKind::ConflictingDiscriminator)
    );

    let ambiguous_message = parse_raw(r#"{"role":"assistant","text":"first","message":"second"}"#);
    assert!(ambiguous_message.rejection.is_none());
    assert!(ambiguous_message.rows.is_empty());
}
