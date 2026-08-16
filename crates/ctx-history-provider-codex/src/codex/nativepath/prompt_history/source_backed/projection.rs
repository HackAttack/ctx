use ctx_history_core::{
    derive_event_id, derive_native_session_id, AgentScope, CoreRecord, EventIdentityInput,
    NativeItemKey, PositionStability, SourceKey, StableEntityId, TypedKey,
};

use super::{
    CodexPromptHistorySourceBackedResultV0, PromptLine, EVENT_POSITION_KIND, LOGICAL_EVENT_KIND,
    LOGICAL_SESSION_KIND, PARSER_REVISION, SESSION_KEY_NAMESPACE,
};

pub(super) fn core_record(
    source: &SourceKey,
    line: PromptLine,
    physical_ordinal: u64,
) -> CodexPromptHistorySourceBackedResultV0<CoreRecord> {
    let session_id = stable_session_id(source, &line.session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        TypedKey::U64(physical_ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let body = prompt_lexical_body(&line.text);
    let occurred_at_unix_ms =
        chrono::DateTime::from_timestamp(line.ts, 0).map(|value| value.timestamp_millis());
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        physical_ordinal,
        "message",
        PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(line.session_id);
    record.native_event_id = Some(TypedKey::U64(physical_ordinal));
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some("user".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn prompt_lexical_body(text: &str) -> String {
    if text.is_empty() {
        "message".to_owned()
    } else {
        text.to_owned()
    }
}

pub(super) fn stable_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexPromptHistorySourceBackedResultV0<StableEntityId> {
    Ok(derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        SESSION_KEY_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?)
}

pub(super) fn retained_record_bytes(record: &CoreRecord) -> usize {
    record
        .content
        .normalized_body
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(record.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(512)
}
