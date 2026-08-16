use super::*;

pub(super) fn complete_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    outputs: &[ClaudeOutputDescriptor],
    value: &Value,
) -> Vec<ClaudeRetainedRow> {
    outputs
        .iter()
        .map(|output| {
            let identity = identity(raw_ordinal, u64::from(output.subrecord_index));
            ClaudeRetainedRow {
                identity,
                native_order: order(identity),
                native_record_id: native_record_id.clone(),
                parent_native_record_id: None,
                kind: ClaudeEventKind::ToolOutput,
                role: Some("tool".to_owned()),
                occurred_at: timestamp.clone(),
                body: None,
                body_sha256: None,
                body_text_retention: None,
                tool_call: None,
                tool_result: Some(ClaudeToolResult {
                    call_id: (!output.call_id_unavailable)
                        .then(|| output.call_id.clone())
                        .flatten(),
                    native_content: output.content.clone().unwrap_or_else(|| value.clone()),
                    call_id_unavailable: output.call_id_unavailable,
                    content_unavailable: output.content_unavailable,
                    native_content_unavailable: output.native_content_unavailable,
                    literal_facts: output.literal_facts.clone(),
                }),
                locator: locator.clone(),
            }
        })
        .collect()
}
