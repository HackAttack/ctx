use super::*;

pub(crate) fn qwen_code_header_session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn qwen_code_header_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn qwen_code_event_type(value: &Value) -> EventType {
    match value.get("type").and_then(Value::as_str) {
        Some("user" | "assistant") if qwen_code_content_has(value, "tool_use") => {
            EventType::ToolCall
        }
        Some("tool_result") => EventType::ToolOutput,
        Some("user" | "assistant") => EventType::Message,
        Some("system") => EventType::Notice,
        _ if value.get("toolCallResult").is_some() => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

pub(crate) fn qwen_code_role(value: &Value) -> EventRole {
    provider_role(
        value
            .pointer("/message/role")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str),
    )
}

pub(crate) fn qwen_code_event_text(value: &Value) -> String {
    value
        .pointer("/message/content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
        .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
        .or_else(|| value.get("content").and_then(provider_value_text))
        .unwrap_or_default()
}

pub(crate) fn qwen_code_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .cloned()
        .or_else(|| value.pointer("/message/model").cloned())
}

fn qwen_code_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
}

pub(crate) fn enumerate_qwen_code_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("tool_result")
        && value.get("toolCallResult").is_none()
    {
        return Ok(Vec::new());
    }
    if reject_redacted(value).is_err() {
        let mut indices = result_block_indices(value.pointer("/message/content"))?;
        if indices.is_empty() {
            indices.push(0);
        }
        return indices
            .into_iter()
            .map(|index| {
                Ok(NativeJsonlResultSubrecord {
                    subrecord_index: u32::try_from(index)
                        .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                    content: None,
                    call_id: None,
                    tool_name: None,
                })
            })
            .collect();
    }
    let blocks = enumerate_content_block_results(value.pointer("/message/content"))?;
    if !blocks.is_empty() && value.get("toolCallResult").is_some() {
        return Err(NativeJsonlResultExtractionError::InvalidShape);
    }
    if !blocks.is_empty() {
        return Ok(blocks);
    }
    if let Some(result) = value.get("toolCallResult") {
        reject_redacted(result)?;
        return Ok(vec![NativeJsonlResultSubrecord {
            subrecord_index: 0,
            content: extract_result_ref(Some(result), &["output", "content", "text"])?,
            call_id: native_result_identity(result).or_else(|| native_result_identity(value)),
            tool_name: native_result_tool_name(result).or_else(|| native_result_tool_name(value)),
        }]);
    }
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content: extract_result_ref(value.get("content"), &[])?,
        call_id: native_result_identity(value),
        tool_name: native_result_tool_name(value),
    }])
}

fn enumerate_content_block_results<'a>(
    content: Option<&'a Value>,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .enumerate()
        .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|(index, block)| {
            let (content, redacted) =
                match extract_result_ref(Some(block), &["content", "output", "text"]) {
                    Ok(content) => (content, false),
                    Err(NativeJsonlResultExtractionError::Redacted) => (None, true),
                    Err(error) => return Err(error),
                };
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content,
                call_id: (!redacted).then(|| native_result_identity(block)).flatten(),
                tool_name: (!redacted)
                    .then(|| native_result_tool_name(block))
                    .flatten(),
            })
        })
        .collect()
}

fn result_block_indices(
    content: Option<&Value>,
) -> std::result::Result<Vec<usize>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    Ok(content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.get("type").and_then(Value::as_str) == Some("tool_result")).then_some(index)
        })
        .collect())
}

fn extract_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> std::result::Result<Option<std::borrow::Cow<'a, str>>, NativeJsonlResultExtractionError> {
    extract_direct_result_content(value, object_fields, true)
}

fn native_result_identity(value: &Value) -> Option<&str> {
    [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_tool_name(value: &Value) -> Option<&str> {
    ["tool_name", "toolName", "name", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn reject_redacted(value: &Value) -> std::result::Result<(), NativeJsonlResultExtractionError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let flag_is_redacted = ["redacted", "is_redacted", "isRedacted"]
        .iter()
        .filter_map(|field| object.get(*field))
        .any(|flag| flag.as_bool() != Some(false));
    let state_is_redacted = ["status", "state"]
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str))
        .any(|state| matches!(state, "redacted" | "output-redacted"));
    if flag_is_redacted || state_is_redacted {
        Err(NativeJsonlResultExtractionError::Redacted)
    } else {
        Ok(())
    }
}
