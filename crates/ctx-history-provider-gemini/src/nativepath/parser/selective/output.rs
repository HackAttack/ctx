use super::*;

#[derive(Default)]
struct RawResultRecord {
    id: Option<Box<RawValue>>,
    timestamp: Option<Box<RawValue>>,
    result: Option<Box<RawValue>>,
    tool_calls: Vec<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for RawResultRecord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawResultVisitor;

        impl<'de> Visitor<'de> for RawResultVisitor {
            type Value = RawResultRecord;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Gemini result record")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut record = RawResultRecord::default();
                let mut id_seen = false;
                let mut timestamp_seen = false;
                let mut result_seen = false;
                let mut tool_calls_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => {
                            reject_duplicate_selector(&mut id_seen, "id")?;
                            record.id = Some(map.next_value()?);
                        }
                        "timestamp" => {
                            reject_duplicate_selector(&mut timestamp_seen, "timestamp")?;
                            record.timestamp = Some(map.next_value()?);
                        }
                        "result" => {
                            reject_duplicate_selector(&mut result_seen, "result")?;
                            record.result = Some(map.next_value()?);
                        }
                        "toolCalls" => {
                            reject_duplicate_selector(&mut tool_calls_seen, "toolCalls")?;
                            record.tool_calls = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(record)
            }
        }

        deserializer.deserialize_map(RawResultVisitor)
    }
}

pub(super) fn parse_result_record_selectively(
    payload: &[u8],
) -> std::result::Result<ProbedGeminiResult, String> {
    let record_audit = audit_json(
        payload,
        gemini_result_record_selector_group,
        gemini_literal_kind_for_key,
    )
    .map_err(|error| format!("invalid Gemini result record: {error}"))?;
    let record: RawResultRecord = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid Gemini result record: {error}"))?;
    let native_record_id = decode_raw_optional_string(record.id.as_deref())?;
    let timestamp = decode_raw_optional_string(record.timestamp.as_deref())?;
    let record_result_unavailable = record_audit.selector_ambiguous(SelectorGroup::Result);
    let record_tool_calls_unavailable = record_audit.selector_ambiguous(SelectorGroup::ToolCalls);
    let mut outputs = Vec::new();

    if let Some(result) = record.result {
        outputs.push(probed_outer_output(
            &result,
            record_result_unavailable,
            record_audit.facts().to_vec(),
        )?);
    }

    for raw_call in record.tool_calls {
        if let Some(output) = probed_call_output(&raw_call, record_tool_calls_unavailable)? {
            outputs.push(output);
        }
        if outputs.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS {
            return Err(format!(
                "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
            ));
        }
    }

    Ok(ProbedGeminiResult {
        native_record_id,
        occurred_at_unix_ms: timestamp
            .as_deref()
            .and_then(parse_timestamp)
            .map(|timestamp| timestamp.timestamp_millis()),
        outputs,
    })
}

fn decode_raw_optional_string(
    raw: Option<&RawValue>,
) -> std::result::Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(raw.get())
        .map_err(|error| format!("invalid Gemini result selector: {error}"))?;
    let (value, invalid) = bounded_native_string(Some(&value));
    if invalid {
        Err("Gemini result selector must be a bounded string or null".to_owned())
    } else {
        Ok(value)
    }
}

fn probed_outer_output(
    raw: &RawValue,
    result_unavailable: bool,
    literal_facts: Vec<ctx_history_core::ProviderDeclaredFact>,
) -> std::result::Result<ProbedGeminiOutput, String> {
    let native_content: Value = serde_json::from_str(raw.get())
        .map_err(|error| format!("invalid Gemini result content: {error}"))?;
    let result = (!result_unavailable).then_some(native_content.clone());
    probed_output(ProbedGeminiOutput {
        native_content,
        result,
        call_id: None,
        tool_name: None,
        arguments: None,
        protocol: None,
        server: None,
        explicit_tool: None,
        call_id_unavailable: false,
        tool_name_unavailable: false,
        arguments_unavailable: false,
        result_unavailable,
        mcp_identity_unavailable: false,
        native_content_unavailable: result_unavailable,
        literal_facts,
        fallback_identity_sha256: [0; 32],
    })
}

fn probed_call_output(
    raw: &RawValue,
    record_tool_calls_unavailable: bool,
) -> std::result::Result<Option<ProbedGeminiOutput>, String> {
    let audit = audit_json(
        raw.get().as_bytes(),
        gemini_selector_group,
        gemini_literal_kind_for_key,
    )
    .map_err(|error| format!("invalid Gemini result tool call: {error}"))?;
    if audit.selector_ambiguous(SelectorGroup::Result) {
        return Err("Gemini result tool call has a duplicate result selector".to_owned());
    }
    let native_content: Value = serde_json::from_str(raw.get())
        .map_err(|error| format!("invalid Gemini result tool call: {error}"))?;
    let object = native_content
        .as_object()
        .ok_or_else(|| "Gemini result tool call must be an object".to_owned())?;
    let (call_id, call_id_invalid) = bounded_native_string(object.get("id"));
    let (tool_name, tool_name_invalid) = bounded_native_string(object.get("name"));
    let (protocol, protocol_invalid) = bounded_native_string(object.get("protocol"));
    let (server, server_invalid) = bounded_native_string(object.get("server"));
    let (explicit_tool, explicit_tool_invalid) = bounded_native_string(object.get("tool"));
    let call_id_unavailable = record_tool_calls_unavailable
        || audit.selector_ambiguous(SelectorGroup::CallId)
        || call_id_invalid;
    let tool_name_unavailable = record_tool_calls_unavailable
        || audit.selector_ambiguous(SelectorGroup::ToolName)
        || tool_name_invalid;
    let arguments_unavailable =
        record_tool_calls_unavailable || audit.selector_ambiguous(SelectorGroup::Arguments);
    let result_unavailable =
        record_tool_calls_unavailable || audit.selector_ambiguous(SelectorGroup::Result);
    let mcp_identity_unavailable = record_tool_calls_unavailable
        || audit.selector_ambiguous(SelectorGroup::Protocol)
        || audit.selector_ambiguous(SelectorGroup::Server)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
        || protocol_invalid
        || server_invalid
        || explicit_tool_invalid;
    let result = (!result_unavailable)
        .then(|| object.get("result").cloned())
        .flatten();
    let arguments = (!arguments_unavailable)
        .then(|| object.get("args").cloned())
        .flatten();
    if result.is_none() && !result_unavailable {
        return Ok(None);
    }
    probed_output(ProbedGeminiOutput {
        native_content,
        result,
        call_id: (!call_id_unavailable).then_some(call_id).flatten(),
        tool_name: (!tool_name_unavailable).then_some(tool_name).flatten(),
        arguments,
        protocol: (!mcp_identity_unavailable).then_some(protocol).flatten(),
        server: (!mcp_identity_unavailable).then_some(server).flatten(),
        explicit_tool: (!mcp_identity_unavailable)
            .then_some(explicit_tool)
            .flatten(),
        call_id_unavailable,
        tool_name_unavailable,
        arguments_unavailable,
        result_unavailable,
        mcp_identity_unavailable,
        native_content_unavailable: record_tool_calls_unavailable
            || audit.any_selector_ambiguous()
            || call_id_invalid
            || tool_name_invalid
            || protocol_invalid
            || server_invalid
            || explicit_tool_invalid,
        literal_facts: audit.facts().to_vec(),
        fallback_identity_sha256: [0; 32],
    })
    .map(Some)
}

fn gemini_result_record_selector_group(key: &str) -> Option<SelectorGroup> {
    match key {
        "id" => Some(SelectorGroup::Invocation),
        "result" => Some(SelectorGroup::Result),
        "toolCalls" => Some(SelectorGroup::ToolCalls),
        _ => None,
    }
}

fn probed_output(
    mut output: ProbedGeminiOutput,
) -> std::result::Result<ProbedGeminiOutput, String> {
    output.fallback_identity_sha256 = result_fallback_identity_sha256(&output)?;
    Ok(output)
}

fn result_fallback_identity_sha256(
    output: &ProbedGeminiOutput,
) -> std::result::Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    hasher.update(RESULT_FALLBACK_ID_DOMAIN);
    hash_page_optional_text(&mut hasher, output.call_id.as_deref());
    let encoded = serde_json::to_vec(&output.native_content)
        .map_err(|error| format!("failed to encode Gemini native result content: {error}"))?;
    hash_page_bytes(&mut hasher, &encoded);
    Ok(hasher.finalize().into())
}

pub(super) fn result_event_identity(
    native_record_id: Option<&str>,
    output: &ProbedGeminiOutput,
    output_index: usize,
) -> GeminiEventIdentity {
    let fallback = hex_sha256(output.fallback_identity_sha256);
    let identity = if let Some(native_record_id) = native_record_id {
        let subrecord = output.call_id.as_deref().map_or_else(
            || format!("fallback-sha256:{fallback}"),
            |call_id| format!("call:{}:{call_id}", call_id.len()),
        );
        format!(
            "gemini-result-v3:record:{}:{native_record_id}:subrecord:{subrecord}:index:{output_index}",
            native_record_id.len()
        )
    } else {
        format!("gemini-result-v3:fallback-sha256:{fallback}:index:{output_index}")
    };
    GeminiEventIdentity::NativeRecordId(identity)
}

pub(super) fn hex_sha256(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_channels_remain_exact_and_in_provider_order() {
        let parsed = parse_result_record_selectively(
            br#"{"id":"native-result","timestamp":"2026-08-16T12:00:00Z","result":{"content":"outer","status":"failed"},"toolCalls":[{"id":"call-1","name":"mcp__forge__read","args":{"command":"  git status  ","path":"./a"},"result":{"content":["first",{"ok":false}]}},{"id":"call-2","name":"plain","args":null,"result":"  exact text  "}]}"#,
        )
        .unwrap();

        assert_eq!(parsed.native_record_id.as_deref(), Some("native-result"));
        assert_eq!(parsed.outputs.len(), 3);
        assert_eq!(
            parsed.outputs[0].result,
            Some(serde_json::json!({"content": "outer", "status": "failed"}))
        );
        assert_eq!(parsed.outputs[1].call_id.as_deref(), Some("call-1"));
        assert_eq!(
            parsed.outputs[1].arguments,
            Some(serde_json::json!({"command": "  git status  ", "path": "./a"}))
        );
        assert_eq!(
            parsed.outputs[1].result,
            Some(serde_json::json!({"content": ["first", {"ok": false}]}))
        );
        assert_eq!(
            parsed.outputs[2].result,
            Some(Value::String("  exact text  ".to_owned()))
        );
    }

    #[test]
    fn duplicate_nonclassification_selectors_withhold_only_affected_channels() {
        let parsed = parse_result_record_selectively(
            br#"{"id":"result","toolCalls":[{"id":"first","id":"second","name":"mcp__server__tool","result":"one"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.outputs.len(), 1);
        let output = &parsed.outputs[0];
        assert!(output.call_id.is_none());
        assert!(output.call_id_unavailable);
        assert_eq!(output.result, Some(Value::String("one".to_owned())));
        assert!(!output.result_unavailable);
        assert_eq!(output.tool_name.as_deref(), Some("mcp__server__tool"));
    }

    #[test]
    fn duplicate_result_containers_are_rejected_before_output_splitting() {
        for payload in [
            br#"{"id":"outer-conflict","result":"one","result":"MUST_NOT_EMIT"}"#.as_slice(),
            br#"{"id":"outer-identical","result":"same","result":"same"}"#.as_slice(),
            br#"{"id":"calls-conflict","toolCalls":[{"result":"one"}],"toolCalls":[{"result":"MUST_NOT_EMIT"}]}"#.as_slice(),
            br#"{"id":"calls-identical","toolCalls":[{"result":"same"}],"toolCalls":[{"result":"same"}]}"#.as_slice(),
            br#"{"id":"nested-conflict","toolCalls":[{"id":"call","result":"one","result":"MUST_NOT_EMIT"}]}"#.as_slice(),
            br#"{"id":"nested-identical","toolCalls":[{"id":"call","result":"same","result":"same"}]}"#.as_slice(),
        ] {
            assert!(parse_result_record_selectively(payload).is_err());
        }
    }

    #[test]
    fn duplicate_result_record_identity_and_time_selectors_are_rejected() {
        for payload in [
            br#"{"id":"first","id":"MUST_NOT_SELECT","result":"value"}"#.as_slice(),
            br#"{"id":"same","id":"same","result":"value"}"#.as_slice(),
            br#"{"id":"result","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T13:00:00Z","result":"value"}"#.as_slice(),
            br#"{"id":"result","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T12:00:00Z","result":"value"}"#.as_slice(),
        ] {
            assert!(parse_result_record_selectively(payload).is_err());
        }
    }
}
