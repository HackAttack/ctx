use super::*;

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn decode_result_record(
    payload: &[u8],
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
) -> std::result::Result<DecodedGeminiResult, String> {
    #[cfg(test)]
    TEST_RESULT_SELECTIVE_PASSES.set(TEST_RESULT_SELECTIVE_PASSES.get().saturating_add(1));
    let result = parse_result_record_selectively(payload)?;
    if result.outputs.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let mut decoded = DecodedGeminiResult { events: Vec::new() };
    for (index, output) in result.outputs.into_iter().enumerate() {
        let sub_ordinal = u32::try_from(index)
            .map_err(|_| "Gemini result subrecord ordinal overflowed".to_owned())?;
        let identity = result_event_identity(result.native_record_id.as_deref(), &output, index);
        let event = decode_tool_result(
            result.occurred_at_unix_ms,
            raw_ordinal,
            sub_ordinal,
            source_record,
            &output,
            identity,
        )?;
        let event_bytes = retained_event_bytes(&event)?;
        decoded.events.push((event.event, event_bytes));
    }
    Ok(decoded)
}

fn decode_tool_result(
    occurred_at_unix_ms: Option<i64>,
    raw_ordinal: u64,
    sub_ordinal: u32,
    source_record: GeminiSourceRecordEvidence,
    output: &ProbedGeminiOutput,
    identity: GeminiEventIdentity,
) -> std::result::Result<DecodedGeminiEvent, String> {
    let body = GeminiEventBody::ToolResult {
        native_content: output.native_content.clone(),
        result: output.result.clone(),
        call_id: output.call_id.clone(),
        tool_name: output.tool_name.clone(),
        arguments: output.arguments.clone(),
        protocol: output.protocol.clone(),
        server: output.server.clone(),
        explicit_tool: output.explicit_tool.clone(),
        call_id_unavailable: output.call_id_unavailable,
        tool_name_unavailable: output.tool_name_unavailable,
        arguments_unavailable: output.arguments_unavailable,
        result_unavailable: output.result_unavailable,
        mcp_identity_unavailable: output.mcp_identity_unavailable,
        native_content_unavailable: output.native_content_unavailable,
        literal_facts: output.literal_facts.clone(),
    };
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode Gemini tool result: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let body_sha256 = hasher.finalize().into();
    Ok(DecodedGeminiEvent {
        event: GeminiRetainedEvent {
            identity,
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal,
            },
            source_record,
            event_type: EventType::ToolOutput,
            role: EventRole::Tool,
            occurred_at: occurred_at_unix_ms.and_then(DateTime::<Utc>::from_timestamp_millis),
            body,
            body_sha256,
            preview: String::new(),
            searchable_text: String::new(),
        },
        serialized_body_bytes: body_bytes.len(),
    })
}

#[derive(Debug, Deserialize)]
struct GeminiStateNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$set")]
    set: GeminiStateSetDto,
}

#[derive(Debug, Deserialize)]
struct GeminiStateSetDto {
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiRewindNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$rewindTo")]
    rewind_to: String,
}

#[derive(Debug)]
pub(in super::super) enum GeminiDecodingError {
    Invalid(String),
}

pub(in super::super) struct DecodedGeminiEvent {
    pub(in super::super) event: GeminiRetainedEvent,
    pub(in super::super) serialized_body_bytes: usize,
}

impl From<String> for GeminiDecodingError {
    fn from(error: String) -> Self {
        Self::Invalid(error)
    }
}

pub(in super::super) fn decode_retained_event(
    payload: &[u8],
    class: GeminiRecordClass,
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
) -> std::result::Result<Vec<DecodedGeminiEvent>, GeminiDecodingError> {
    if class == GeminiRecordClass::ToolCall {
        return decode_tool_call_events(payload, raw_ordinal, source_record);
    }
    let (id, occurred_at, event_type, role, body, searchable_text) = match class {
        GeminiRecordClass::Message => {
            let dto: GeminiMessageDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini message: {error}"))?;
            let Some(text) = dto.content.filter(|text| !text.is_empty()) else {
                return Ok(Vec::new());
            };
            let role = match dto.record_type.as_deref() {
                Some("user") => EventRole::User,
                Some("gemini") => EventRole::Assistant,
                _ => return Ok(Vec::new()),
            };
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Message,
                role,
                GeminiEventBody::Message {
                    text: text.clone(),
                    model: dto.model,
                },
                text,
            )
        }
        GeminiRecordClass::ToolCall => unreachable!("handled above"),
        GeminiRecordClass::StateNotice => {
            let dto: GeminiStateNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini state notice: {error}"))?;
            let summary = dto.set.summary;
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::StateNotice {
                    summary: summary.clone(),
                },
                summary.unwrap_or_default(),
            )
        }
        GeminiRecordClass::RewindNotice => {
            let dto: GeminiRewindNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini rewind notice: {error}"))?;
            let target = dto.rewind_to.trim().to_owned();
            if target.is_empty() {
                return Ok(Vec::new());
            }
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::RewindNotice {
                    target_native_record_id: target.clone(),
                },
                format!("rewind to {target}"),
            )
        }
        GeminiRecordClass::Header | GeminiRecordClass::Result | GeminiRecordClass::Ignored => {
            return Ok(Vec::new())
        }
    };

    Ok(vec![build_decoded_event(
        GeminiEventIdentity::NativeRecordId(id),
        raw_ordinal,
        0,
        source_record,
        event_type,
        role,
        occurred_at,
        body,
        searchable_text,
    )?])
}

fn decode_tool_call_events(
    payload: &[u8],
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
) -> std::result::Result<Vec<DecodedGeminiEvent>, GeminiDecodingError> {
    let record_audit = audit_json(
        payload,
        gemini_tool_call_record_selector_group,
        gemini_literal_kind_for_key,
    )
    .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
    let dto: GeminiToolCallRecordDto = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
    let native_record_id = required_record_id(dto.id)?;
    let occurred_at = dto.timestamp.as_deref().and_then(parse_timestamp);
    let _ = dto.content;
    let record_selectors_unavailable = record_audit.selector_ambiguous(SelectorGroup::Type)
        || record_audit.selector_ambiguous(SelectorGroup::ToolCalls);
    let mut events = Vec::with_capacity(dto.tool_calls.len());
    for (index, raw_call) in dto.tool_calls.into_iter().enumerate() {
        let call = decode_native_tool_call(&raw_call, record_selectors_unavailable)?;
        if call.native_content.get("result").is_some() {
            return Err(GeminiDecodingError::Invalid(
                "Gemini result-bearing tool call reached invocation decoding".to_owned(),
            ));
        }
        let sub_ordinal = u32::try_from(index)
            .map_err(|_| "Gemini tool-call subrecord ordinal overflowed".to_owned())?;
        let identity = tool_call_event_identity(&native_record_id, &call, index);
        let searchable_text = tool_call_search_text(std::slice::from_ref(&call));
        events.push(build_decoded_event(
            identity,
            raw_ordinal,
            sub_ordinal,
            source_record,
            EventType::ToolCall,
            EventRole::Assistant,
            occurred_at,
            GeminiEventBody::ToolCall { calls: vec![call] },
            searchable_text,
        )?);
    }
    Ok(events)
}

fn tool_call_event_identity(
    native_record_id: &str,
    call: &GeminiToolCall,
    index: usize,
) -> GeminiEventIdentity {
    let subrecord = call.id.as_deref().map_or_else(
        || format!("index:{index}"),
        |call_id| format!("call:{}:{call_id}:index:{index}", call_id.len()),
    );
    GeminiEventIdentity::NativeRecordId(format!(
        "gemini-call-v1:record:{}:{native_record_id}:subrecord:{subrecord}",
        native_record_id.len()
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_decoded_event(
    identity: GeminiEventIdentity,
    raw_ordinal: u64,
    sub_ordinal: u32,
    source_record: GeminiSourceRecordEvidence,
    event_type: EventType,
    role: EventRole,
    occurred_at: Option<DateTime<Utc>>,
    body: GeminiEventBody,
    searchable_text: String,
) -> std::result::Result<DecodedGeminiEvent, String> {
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode retained Gemini body: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let body_sha256 = hasher.finalize().into();
    let preview = searchable_text
        .chars()
        .take(PROVIDER_MAX_PREVIEW_CHARS)
        .collect();
    Ok(DecodedGeminiEvent {
        event: GeminiRetainedEvent {
            identity,
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal,
            },
            source_record,
            event_type,
            role,
            occurred_at,
            body,
            body_sha256,
            preview,
            searchable_text,
        },
        serialized_body_bytes: body_bytes.len(),
    })
}

pub(in super::super) fn retained_event_bytes(
    event: &DecodedGeminiEvent,
) -> std::result::Result<usize, String> {
    let mut total = EVENT_ENVELOPE_FIXED_BYTES
        .checked_add(event.serialized_body_bytes)
        .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    let GeminiEventIdentity::NativeRecordId(identity) = &event.event.identity;
    for value in [
        identity.as_str(),
        event.event.preview.as_str(),
        event.event.searchable_text.as_str(),
    ]
    .into_iter()
    {
        total =
            total
                .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                    "Gemini retained event string byte count overflowed".to_owned()
                })?)
                .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    }
    Ok(total)
}

fn required_record_id(id: Option<String>) -> std::result::Result<String, String> {
    nonempty(id).ok_or_else(|| "Gemini event is missing a nonempty native id".to_owned())
}

pub(in super::super) fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub(super) fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn tool_call_search_text(calls: &[GeminiToolCall]) -> String {
    let mut text = String::new();
    for call in calls {
        if let Some(unit) = call.name.as_deref() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(unit);
        }
        if let Some(unit) = call
            .args
            .as_ref()
            .and_then(|args| serde_json::to_string(args).ok())
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&unit);
        }
    }
    text
}
