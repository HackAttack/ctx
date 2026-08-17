use super::*;
use std::borrow::Cow;

pub(super) fn codex_invocation_discovery_exclusion(
    payload: &Value,
    audit: &RawJsonAudit,
    activity: Option<&CoreActivity>,
) -> Option<CoreDiscoveryExclusion> {
    if audit.selector_ambiguous(SelectorGroup::Type)
        || audit.selector_ambiguous(SelectorGroup::CallId)
        || audit.selector_ambiguous(SelectorGroup::ToolName)
        || audit.selector_ambiguous(SelectorGroup::Arguments)
        || payload.get("type").and_then(Value::as_str) != Some("function_call")
        || payload.get("tool").is_some()
    {
        return None;
    }
    let invocation = activity?.invocation.as_ref()?;
    if !ctx_history_capture_model::tool_input::is_command_tool(&invocation.tool) {
        return None;
    }
    let ActivityJsonCapture::Present { value } = &invocation.arguments else {
        return None;
    };
    ctx_history_capture_model::ctx_retrieval::discovery_exclusion_for([
        ctx_history_capture_model::ctx_retrieval::classify_direct_cli_tool_input(value),
    ])
}

pub(super) fn codex_result_discovery_exclusion(
    raw_record: &[u8],
    linked_invocation_discovery_exclusion: Option<CoreDiscoveryExclusion>,
    source_unique_terminal: bool,
    activity: Option<&CoreActivity>,
) -> Option<CoreDiscoveryExclusion> {
    if !source_unique_terminal {
        return None;
    }
    let contribution =
        exact_mcp_terminal_result_contribution(raw_record, activity).unwrap_or_else(|| {
            let exact_success = activity
                .and_then(|activity| activity.provider_call_id.as_ref())
                .is_some_and(|call_id| {
                    let TypedKey::Utf8(call_id) = call_id else {
                        return false;
                    };
                    codex_exact_successful_function_output(raw_record, call_id)
                });
            let linked_invocation = linked_invocation_discovery_exclusion.map(|_| {
                ctx_history_capture_model::ctx_retrieval::ContributionClass::RetrievalDerived
            });
            ctx_history_capture_model::ctx_retrieval::classify_linked_result(
                linked_invocation,
                if exact_success {
                    ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Succeeded
                } else {
                    ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Unknown
                },
                if exact_success {
                    [
                        ctx_history_capture_model::ctx_retrieval::ResultAtom::KnownProviderEnvelope,
                        ctx_history_capture_model::ctx_retrieval::ResultAtom::Payload,
                    ]
                } else {
                    [
                        ctx_history_capture_model::ctx_retrieval::ResultAtom::Unknown,
                        ctx_history_capture_model::ctx_retrieval::ResultAtom::Unknown,
                    ]
                },
            )
        });
    ctx_history_capture_model::ctx_retrieval::discovery_exclusion_for([contribution])
}

fn exact_mcp_terminal_result_contribution(
    raw_record: &[u8],
    activity: Option<&CoreActivity>,
) -> Option<ctx_history_capture_model::ctx_retrieval::ContributionClass> {
    let activity = activity?;
    let TypedKey::Utf8(provider_call_id) = activity.provider_call_id.as_ref()? else {
        return None;
    };
    let invocation = activity.invocation.as_ref()?;
    let exact: ExactMcpTerminalEnvelope<'_> = serde_json::from_slice(raw_record).ok()?;
    if exact.record_type != "event_msg"
        || exact.payload.item_type != "mcp_tool_call_end"
        || exact.payload.call_id.as_ref() != provider_call_id.as_str()
        || Some(exact.payload.invocation.server.as_ref()) != invocation.server.as_deref()
        || exact.payload.invocation.tool != invocation.tool
        || exact.timestamp.as_deref().is_some_and(str::is_empty)
        || exact
            .payload
            .duration
            .as_ref()
            .is_some_and(|duration| duration.nanos >= 1_000_000_000)
    {
        return None;
    }
    let linked_invocation = Some(
        ctx_history_capture_model::ctx_retrieval::classify_mcp_invocation(
            &exact.payload.invocation.server,
            &exact.payload.invocation.tool,
        ),
    );
    let (terminal_status, has_payload) =
        match (exact.payload.result.success, exact.payload.result.error) {
            (Some(success), None) => {
                if success.content.is_empty()
                    || success
                        .content
                        .iter()
                        .any(|block| block.block_type != "text" || block.text.is_empty())
                {
                    return None;
                }
                (
                    if success.is_error {
                        ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Failed
                    } else {
                        ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Succeeded
                    },
                    true,
                )
            }
            (None, Some(_)) => (
                ctx_history_capture_model::ctx_retrieval::ResultTerminalStatus::Failed,
                true,
            ),
            (Some(_), Some(_)) | (None, None) => return None,
        };
    Some(
        ctx_history_capture_model::ctx_retrieval::classify_linked_result(
            linked_invocation,
            terminal_status,
            if has_payload {
                [
                    ctx_history_capture_model::ctx_retrieval::ResultAtom::KnownProviderEnvelope,
                    ctx_history_capture_model::ctx_retrieval::ResultAtom::Payload,
                ]
            } else {
                [
                    ctx_history_capture_model::ctx_retrieval::ResultAtom::KnownProviderEnvelope,
                    ctx_history_capture_model::ctx_retrieval::ResultAtom::Unknown,
                ]
            },
        ),
    )
}

const MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES: usize = 1024 * 1024;

fn codex_exact_successful_function_output(record: &[u8], expected_call_id: &str) -> bool {
    let Ok(envelope) = serde_json::from_slice::<ExactFunctionOutputEnvelope<'_>>(record) else {
        return false;
    };
    envelope.record_type == "response_item"
        && envelope.payload.item_type == "function_call_output"
        && !envelope.payload.call_id.is_empty()
        && envelope.payload.call_id == expected_call_id
        && envelope
            .payload
            .status
            .as_deref()
            .is_none_or(|status| status == "success")
        && envelope
            .timestamp
            .as_deref()
            .is_none_or(|timestamp| !timestamp.is_empty())
        && exact_codex_exec_result_body(&envelope.payload.output).is_some()
}

fn exact_codex_exec_result_body(output: &str) -> Option<&str> {
    if output.is_empty()
        || output.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || output.contains('\0')
    {
        return None;
    }
    if let Some(remainder) = output.strip_prefix("Script completed\n") {
        return exact_codex_exec_result_tail(remainder);
    }
    let (chunk_id, remainder) = output.strip_prefix("Chunk ID: ")?.split_once('\n')?;
    if chunk_id.len() != 6
        || !chunk_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    let (wall_time, remainder) = remainder
        .strip_prefix("Wall time: ")?
        .split_once(" seconds\n")?;
    if wall_time.is_empty() || wall_time.len() > 32 {
        return None;
    }
    let mut wall_time_components = wall_time.split('.');
    let whole = wall_time_components.next()?;
    let fractional = wall_time_components.next();
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fractional.is_some_and(|value| {
            value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        })
        || wall_time_components.next().is_some()
        || wall_time
            .parse::<f64>()
            .ok()
            .is_none_or(|seconds| !seconds.is_finite())
    {
        return None;
    }
    exact_codex_exec_result_tail(remainder)
}

fn exact_codex_exec_result_tail(remainder: &str) -> Option<&str> {
    let remainder = remainder.strip_prefix("Process exited with code 0\n")?;
    let body = if let Some(remainder) = remainder.strip_prefix("Original token count: ") {
        let (token_count, remainder) = remainder.split_once('\n')?;
        if token_count.is_empty()
            || token_count.len() > 20
            || !token_count.bytes().all(|byte| byte.is_ascii_digit())
            || token_count.parse::<u64>().is_err()
        {
            return None;
        }
        remainder.strip_prefix("Output:\n")?
    } else {
        remainder.strip_prefix("Final output:\n")?
    };
    if body.is_empty()
        || body.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || body.lines().any(|line| {
            let line = line.trim();
            line.starts_with("Chunk ID: ")
                || line.starts_with("Wall time: ")
                || line.starts_with("Process exited with code ")
                || line.starts_with("Original token count: ")
                || line == "Output:"
                || line == "Final output:"
                || line.starts_with("Warning: truncated output (original token count: ")
                || line.starts_with("Warning: truncated output (original char count: ")
        })
    {
        return None;
    }
    Some(body)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFunctionOutputEnvelope<'a> {
    #[serde(default, borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    record_type: Cow<'a, str>,
    #[serde(borrow)]
    payload: ExactFunctionOutputPayload<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFunctionOutputPayload<'a> {
    #[serde(rename = "type", borrow)]
    item_type: Cow<'a, str>,
    #[serde(borrow)]
    call_id: Cow<'a, str>,
    #[serde(default, borrow)]
    status: Option<Cow<'a, str>>,
    #[serde(borrow)]
    output: Cow<'a, str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpTerminalEnvelope<'a> {
    #[serde(default, borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    record_type: Cow<'a, str>,
    #[serde(borrow)]
    payload: ExactMcpTerminalPayload<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpTerminalPayload<'a> {
    #[serde(rename = "type", borrow)]
    item_type: Cow<'a, str>,
    #[serde(borrow)]
    call_id: Cow<'a, str>,
    #[serde(borrow)]
    invocation: ExactMcpTerminalInvocation<'a>,
    #[serde(default, deserialize_with = "deserialize_present_mcp_duration")]
    duration: Option<ExactMcpDuration>,
    #[serde(borrow)]
    result: ExactMcpTerminalResult<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpDuration {
    #[serde(rename = "secs")]
    _secs: u64,
    nanos: u64,
}

fn deserialize_present_mcp_duration<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ExactMcpDuration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    ExactMcpDuration::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpTerminalInvocation<'a> {
    #[serde(borrow)]
    server: Cow<'a, str>,
    #[serde(borrow)]
    tool: Cow<'a, str>,
    #[serde(rename = "arguments")]
    _arguments: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpTerminalResult<'a> {
    #[serde(rename = "Ok", default)]
    #[serde(borrow)]
    success: Option<ExactMcpSuccessfulResult<'a>>,
    #[serde(rename = "Err", default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpSuccessfulResult<'a> {
    #[serde(borrow)]
    content: Vec<ExactMcpTextContent<'a>>,
    #[serde(
        rename = "structuredContent",
        default,
        deserialize_with = "deserialize_present_structured_content"
    )]
    _structured_content: Option<serde_json::Map<String, Value>>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMcpTextContent<'a> {
    #[serde(rename = "type", borrow)]
    block_type: Cow<'a, str>,
    #[serde(borrow)]
    text: Cow<'a, str>,
}

fn deserialize_present_structured_content<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Map<String, Value>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Map::<String, Value>::deserialize(deserializer).map(Some)
}
