use super::*;

pub(super) fn core_record<R: crate::JsonlProviderRuntime>(
    compound: &KimiCompoundObservation,
    session_id: StableEntityId,
    fallback_identities: &mut FallbackEventIdentityState<R>,
    bytes: &[u8],
    ordinal: u64,
    value: &Value,
    fallback_timestamp: DateTime<Utc>,
) -> KimiSourceBackedResult<Option<CoreRecord>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((event_type, body)) =
        kimi_lexical_body(value, ordinal, compound.native.session.cwd.as_deref())?
    else {
        return Ok(None);
    };
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let assignment = fallback_identities.assign(fallback_fingerprint(bytes)?, None)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &compound.source,
        session_id,
        logical_item_kind: KIMI_LOGICAL_EVENT_KIND,
        native_item_key: assignment.native_item_key(),
        subrecord_selector: None,
    })?;
    let mut facts = kimi_literal_facts(value)?;
    let parent_session_id = compound
        .native
        .session
        .parent_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?;
    let root_session_id = compound
        .native
        .session
        .root_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?;
    if let Some(cwd) = &compound.native.session.cwd {
        facts.insert(
            0,
            ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: cwd.clone(),
            },
        );
    }
    let event = value.get("event").unwrap_or(value);
    let activity = kimi_activity(event, event_type, &body, facts)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        compound.source.clone(),
        ordinal,
        event_type.as_str(),
        KIMI_SOURCE_PARSER_REVISION,
        body.clone(),
    )?;
    if let Some(parent_session_id) = parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.root_session_id = root_session_id;
        if compound.native.session.agent_scope == Some(ctx_history_core::AgentScope::Subagent) {
            record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        }
    }
    record.provider_session_id = Some(compound.native.session.provider_session_id.clone());
    record.native_event_id = Some(assignment.native_event_id().clone());
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = Some(role.as_str().to_owned());
    record.agent_scope = compound.native.session.agent_scope;
    record.content.structured_content = Some(value.clone());
    record.content.activity = activity;
    ctx_history_jsonl::fit_jsonl_activity(
        &body,
        record.content.structured_content.as_ref(),
        &mut record.content.activity,
        ctx_history_jsonl::JsonlActivityObservedBytes::infer_from_present(),
        MAX_CORE_CONTENT_BYTES,
    );
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(Some(record))
}

fn kimi_activity(
    event: &Value,
    event_type: EventType,
    body: &str,
    facts: Vec<ProviderDeclaredFact>,
) -> KimiSourceBackedResult<Option<CoreActivity>> {
    let call_ids = ["toolCallId", "callId", "call_id", "id"]
        .into_iter()
        .filter_map(|field| event.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let provider_call_id = match call_ids.as_slice() {
        [id] if !id.is_empty() => Some(TypedKey::utf8(*id)?),
        _ => None,
    };
    let tools = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| event.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let invocation = if event_type == EventType::ToolCall {
        match tools.as_slice() {
            [tool] if !tool.is_empty() => Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool: (*tool).to_owned(),
                arguments: event.get("args").or_else(|| event.get("arguments")).map_or(
                    ActivityJsonCapture::Absent,
                    |value| ActivityJsonCapture::Present {
                        value: value.clone(),
                    },
                ),
                started_at_unix_ms: None,
            }),
            _ => None,
        }
    } else {
        None
    };
    let result =
        matches!(event_type, EventType::ToolOutput | EventType::CommandOutput).then(|| {
            ActivityResult {
                status: None,
                completed_at_unix_ms: None,
                duration_ns: None,
                text: ActivityTextCapture::Present {
                    value: body.to_owned(),
                },
                structured_content: ActivityJsonCapture::Present {
                    value: event.clone(),
                },
            }
        });
    if provider_call_id.is_none() && invocation.is_none() && result.is_none() && facts.is_empty() {
        return Ok(None);
    }
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}

pub(super) fn kimi_lexical_body(
    value: &Value,
    _ordinal: u64,
    _cwd: Option<&str>,
) -> KimiSourceBackedResult<Option<(EventType, String)>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = kimi_event_type(record_type, value);
    let body = if event_type == EventType::ToolOutput {
        kimi_output_content(value).unwrap_or_default()
    } else {
        kimi_event_text(record_type, value, event_type)
    };
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some((event_type, body)))
}

fn kimi_literal_facts(value: &Value) -> KimiSourceBackedResult<Vec<ProviderDeclaredFact>> {
    let mut facts = Vec::new();
    let outcome = visit_provider_file_reference_drafts_with_limit(
        value,
        MAX_PROVIDER_FILE_REFERENCES_PER_EVENT,
        |(_, reference)| {
            facts.push(ProviderDeclaredFact {
                kind: reference.kind,
                value: reference.value,
            });
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(if outcome.limit_exceeded() {
        Vec::new()
    } else {
        facts
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_tool_call_id_is_published_for_call_and_result() {
        let call = serde_json::json!({
            "type": "tool.call",
            "toolCallId": "call_1",
            "name": "Read",
            "args": {"path": "/tmp/splines.txt"}
        });
        let result = serde_json::json!({
            "type": "tool.result",
            "toolCallId": "call_1",
            "result": {"output": "spline data", "isError": false}
        });

        for (event, event_type) in [
            (&call, EventType::ToolCall),
            (&result, EventType::ToolOutput),
        ] {
            let activity = kimi_activity(event, event_type, "body", Vec::new())
                .unwrap()
                .unwrap();
            assert_eq!(
                activity.provider_call_id,
                Some(TypedKey::utf8("call_1").unwrap())
            );
            assert_eq!(
                activity
                    .result
                    .as_ref()
                    .and_then(|result| result.status.as_deref()),
                None
            );
        }
    }

    #[test]
    fn provider_textual_result_over_16k_is_complete() {
        let tail = "kimi_success_result_tail_complete";
        let output = format!("{} {tail}", "successful kimi output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolName": "bash",
                "call_id": "complete-success",
                "exit_code": 0,
                "output": output,
            }
        });

        let (event_type, body) = kimi_lexical_body(&value, 0, None).unwrap().unwrap();
        assert_eq!(event_type, EventType::ToolOutput);
        assert_eq!(body, output);
        assert!(body.ends_with(tail));
    }
}
