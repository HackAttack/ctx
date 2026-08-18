use super::*;

pub(super) fn mistral_vibe_activity(
    value: &Value,
    event_type: EventType,
    body: &str,
    facts: Vec<ProviderDeclaredFact>,
) -> Option<CoreActivity> {
    let provider_call_id = admit_optional_provider_call_id(
        value
            .get("tool_call_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    );
    let invocation = if event_type == EventType::ToolCall && provider_call_id.is_some() {
        admit_optional_metadata_text(
            value
                .get("name")
                .or_else(|| value.get("tool_name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
        .map(|tool| ActivityInvocation {
            protocol: None,
            server: None,
            tool,
            arguments: exact_json_capture(value, &["arguments", "input"]),
            started_at_unix_ms: None,
        })
    } else {
        None
    };
    let result = (event_type == EventType::ToolOutput && provider_call_id.is_some()).then(|| {
        ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present {
                value: body.to_owned(),
            },
            structured_content: ActivityJsonCapture::Present {
                value: value.clone(),
            },
        }
    });
    (invocation.is_some() || result.is_some() || !facts.is_empty()).then_some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    })
}

fn exact_json_capture(value: &Value, fields: &[&str]) -> ActivityJsonCapture {
    let candidates = fields
        .iter()
        .filter_map(|field| value.get(*field))
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => ActivityJsonCapture::Absent,
        [candidate] => ActivityJsonCapture::Present {
            value: (*candidate).clone(),
        },
        _ => ActivityJsonCapture::Unavailable,
    }
}
