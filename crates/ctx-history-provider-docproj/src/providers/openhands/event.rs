use std::{collections::BTreeSet, fmt, path::Path};

use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_capture_model::{
    normalization::{provider_explicit_result_value_text, provider_role, provider_value_text},
    time::parse_rfc3339_utc,
};
use ctx_history_core::{EventRole, EventType};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsDecodedEvent {
    event_id: String,
    timestamp: DateTime<Utc>,
    event_type: EventType,
    role: EventRole,
    text: String,
    value: Value,
    capture_audit: OpenHandsCaptureAudit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OpenHandsCaptureAudit {
    pub(crate) duplicate_key: bool,
    pub(crate) discriminator_alias_conflict: bool,
    pub(crate) call_id_alias_conflict: bool,
    pub(crate) tool_name_alias_conflict: bool,
    pub(crate) arguments_alias_conflict: bool,
    pub(crate) result_alias_conflict: bool,
    pub(crate) status_alias_conflict: bool,
}

impl OpenHandsDecodedEvent {
    pub(crate) fn event_id(&self) -> &str {
        &self.event_id
    }

    pub(crate) fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    pub(crate) fn event_type(&self) -> EventType {
        self.event_type
    }

    pub(crate) fn role(&self) -> EventRole {
        self.role
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn value(&self) -> &Value {
        &self.value
    }

    pub(crate) fn capture_audit(&self) -> OpenHandsCaptureAudit {
        self.capture_audit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenHandsEventDecodeErrorKind {
    Invalid,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenHandsEventDecodeError {
    kind: OpenHandsEventDecodeErrorKind,
    message: String,
}

impl OpenHandsEventDecodeError {
    fn invalid(message: String) -> Self {
        Self {
            kind: OpenHandsEventDecodeErrorKind::Invalid,
            message,
        }
    }

    fn too_large(observed_bytes: usize) -> Self {
        Self {
            kind: OpenHandsEventDecodeErrorKind::TooLarge,
            message: format!(
                "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {observed_bytes} bytes)"
            ),
        }
    }
}

impl fmt::Display for OpenHandsEventDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OpenHandsEventDecodeError {}

/// Decodes one bounded OpenHands event file into its authoritative semantics.
///
/// Import and Core projection consume this result so event identity, type,
/// role, and text cannot drift.
pub(crate) fn decode_openhands_event(
    path: &Path,
    bytes: &[u8],
) -> Result<OpenHandsDecodedEvent, OpenHandsEventDecodeError> {
    if bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        return Err(OpenHandsEventDecodeError::too_large(bytes.len()));
    }
    let value = serde_json::from_slice::<Value>(bytes).map_err(|error| {
        OpenHandsEventDecodeError::invalid(format!("invalid OpenHands event JSON: {error}"))
    })?;
    let duplicate_keys = openhands_duplicate_json_keys(bytes).map_err(|error| {
        OpenHandsEventDecodeError::invalid(format!("invalid OpenHands event JSON: {error}"))
    })?;
    if let Some(key) = duplicate_keys.iter().find(|key| {
        matches!(
            key.as_str(),
            "id" | "timestamp"
                | "kind"
                | "type"
                | "source"
                | "role"
                | "action"
                | "observation"
                | "llm_message"
                | "tool_call_id"
                | "toolCallId"
                | "tool_use_id"
                | "toolUseId"
                | "name"
                | "tool"
                | "tool_name"
                | "toolName"
                | "status"
                | "state"
                | "outcome"
        )
    }) {
        return Err(OpenHandsEventDecodeError::invalid(format!(
            "OpenHands event has duplicate critical selector {key:?}"
        )));
    }
    let raw_json = std::str::from_utf8(bytes)
        .map_err(|error| OpenHandsEventDecodeError::invalid(error.to_string()))?
        .to_owned();
    decode_openhands_event_value_with_raw(path, value, raw_json, !duplicate_keys.is_empty())
}

/// Applies the authoritative OpenHands semantics to an already parsed event.
///
/// Complete-content recovery parses through its stricter shared JSON budget
/// before calling this entry point. Live import retains its existing byte-only
/// admission contract through [`decode_openhands_event`].
fn decode_openhands_event_value_with_raw(
    path: &Path,
    value: Value,
    raw_json: String,
    duplicate_key: bool,
) -> Result<OpenHandsDecodedEvent, OpenHandsEventDecodeError> {
    let capture_audit = openhands_capture_audit(&value, duplicate_key);
    if capture_audit.discriminator_alias_conflict {
        return Err(OpenHandsEventDecodeError::invalid(
            "OpenHands event has conflicting kind/type selectors".to_owned(),
        ));
    }
    let event_id =
        super::openhands_bounded_derived_text(openhands_event_id(path, &value), "event id")
            .map_err(|error| OpenHandsEventDecodeError::invalid(error.to_string()))?;
    let timestamp = openhands_event_timestamp(&value).ok_or_else(|| {
        OpenHandsEventDecodeError::invalid(format!(
            "OpenHands event {event_id} missing valid timestamp"
        ))
    })?;
    let entry_type = openhands_entry_type(&value);
    let event_type = openhands_event_type(&value, &entry_type);
    let role = openhands_role(&value, &entry_type);
    let text = if capture_audit.duplicate_key
        || capture_audit.discriminator_alias_conflict
        || capture_audit.call_id_alias_conflict
        || capture_audit.tool_name_alias_conflict
        || capture_audit.arguments_alias_conflict
        || capture_audit.result_alias_conflict
        || capture_audit.status_alias_conflict
    {
        raw_json.clone()
    } else {
        openhands_event_text(&value, &entry_type, event_type)?
    };
    Ok(OpenHandsDecodedEvent {
        event_id,
        timestamp,
        event_type,
        role,
        text,
        value,
        capture_audit,
    })
}

fn openhands_capture_audit(value: &Value, duplicate_key: bool) -> OpenHandsCaptureAudit {
    let action = value.get("action").and_then(Value::as_object);
    let observation = value.get("observation").and_then(Value::as_object);
    OpenHandsCaptureAudit {
        duplicate_key,
        discriminator_alias_conflict: json_aliases_conflict(value, &["kind", "type"]),
        call_id_alias_conflict: json_aliases_conflict(
            value,
            &["tool_call_id", "toolCallId", "tool_use_id", "toolUseId"],
        ),
        tool_name_alias_conflict: action.is_some_and(|action| {
            json_aliases_conflict_map(action, &["kind", "name", "tool", "tool_name", "toolName"])
        }),
        arguments_alias_conflict: action.is_some_and(|action| {
            json_aliases_conflict_map(action, &["arguments", "args", "input", "parameters"])
        }),
        result_alias_conflict: observation.is_some_and(|observation| {
            json_aliases_conflict_map(observation, &["content", "output", "result"])
        }),
        status_alias_conflict: observation.is_some_and(|observation| {
            json_aliases_conflict_map(observation, &["status", "state", "outcome"])
        }),
    }
}

fn json_aliases_conflict(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| json_aliases_conflict_map(object, keys))
}

fn json_aliases_conflict_map(object: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    let mut selected = None;
    for key in keys {
        let Some(candidate) = object.get(*key).filter(|value| !value.is_null()) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return true;
        }
        selected = Some(candidate);
    }
    false
}

fn openhands_duplicate_json_keys(bytes: &[u8]) -> Result<BTreeSet<String>, serde_json::Error> {
    let mut duplicates = BTreeSet::new();
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DuplicateKeySeed(&mut duplicates).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(duplicates)
}

struct DuplicateKeySeed<'a>(&'a mut BTreeSet<String>);

impl<'de> DeserializeSeed<'de> for DuplicateKeySeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateKeyVisitor(self.0))
    }
}

struct DuplicateKeyVisitor<'a>(&'a mut BTreeSet<String>);

impl<'de> Visitor<'de> for DuplicateKeyVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(DuplicateKeySeed(self.0))?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                self.0.insert(key);
            }
            map.next_value_seed(DuplicateKeySeed(self.0))?;
        }
        Ok(())
    }
}

fn openhands_event_id(path: &Path, value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

fn openhands_event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| {
            parse_rfc3339_utc(timestamp).or_else(|| {
                NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.f")
                    .ok()
                    .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
            })
        })
}

fn openhands_entry_type(value: &Value) -> String {
    if let Some(entry_type) = value
        .get("kind")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
    {
        return entry_type.to_owned();
    }
    if value.get("llm_message").is_some() {
        "MessageEvent".to_owned()
    } else if value.get("action").is_some() {
        "ActionEvent".to_owned()
    } else if value.get("observation").is_some() {
        "ObservationEvent".to_owned()
    } else {
        "OpenHandsEvent".to_owned()
    }
}

fn openhands_event_type(value: &Value, entry_type: &str) -> EventType {
    // Some OpenHands records carry an llm_message/action alongside the
    // resulting observation. The observation is the authoritative event in
    // that combined shape; classifying it as a message or tool call can leak
    // successful command output into the lexical body.
    if value.get("observation").is_some() || entry_type == "ObservationEvent" {
        return match value.pointer("/observation/kind").and_then(Value::as_str) {
            Some(
                "FileEditorObservation"
                | "StrReplaceEditorObservation"
                | "PlanningFileEditorObservation",
            ) => EventType::ToolOutput,
            Some("ExecuteBashObservation" | "TerminalObservation") => EventType::CommandOutput,
            _ => EventType::ToolOutput,
        };
    }
    if value.get("action").is_some() || entry_type == "ActionEvent" {
        return match value.pointer("/action/kind").and_then(Value::as_str) {
            Some("FinishAction") => EventType::Message,
            Some("ThinkAction") => EventType::Summary,
            Some("FileEditorAction" | "StrReplaceEditorAction" | "PlanningFileEditorAction") => {
                EventType::ToolCall
            }
            _ => EventType::ToolCall,
        };
    }
    if value.get("llm_message").is_some() || entry_type == "MessageEvent" {
        return EventType::Message;
    }
    match entry_type {
        "StreamingDeltaEvent" => EventType::Message,
        "CondensationSummaryEvent" | "CondensationEvent" => EventType::Summary,
        "AgentErrorEvent" | "ConversationErrorEvent" | "ServerErrorEvent" => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

fn openhands_role(value: &Value, entry_type: &str) -> EventRole {
    if let Some(role) = value.pointer("/llm_message/role").and_then(Value::as_str) {
        return provider_role(Some(role));
    }
    match value.get("source").and_then(Value::as_str) {
        Some("user") => EventRole::User,
        Some("agent") => EventRole::Assistant,
        Some("environment" | "hook") => EventRole::Tool,
        Some(source) => provider_role(Some(source)),
        None if entry_type == "ActionEvent" => EventRole::Assistant,
        None if entry_type == "ObservationEvent" => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

fn openhands_event_text(
    value: &Value,
    entry_type: &str,
    event_type: EventType,
) -> Result<String, OpenHandsEventDecodeError> {
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        let candidates = ["content", "output", "result"]
            .into_iter()
            .filter_map(|field| value.pointer(&format!("/observation/{field}")))
            .filter(|value| !value.is_null())
            .collect::<Vec<_>>();
        let selected = match candidates.as_slice() {
            [] => None,
            [selected] => Some(*selected),
            _ => {
                return Err(OpenHandsEventDecodeError::invalid(
                    "OpenHands observation exposes more than one candidate result body field"
                        .to_owned(),
                ));
            }
        };
        if let Some(content) = selected.and_then(provider_explicit_result_value_text) {
            return Ok(content);
        }
        if let Some(error) = value
            .pointer("/observation/error")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
        {
            return Ok(error.to_owned());
        }
    }
    if let Some(text) = value
        .pointer("/llm_message/content")
        .and_then(provider_explicit_result_value_text)
    {
        return Ok(text);
    }
    if let Some(text) = value.get("content").and_then(provider_value_text) {
        return Ok(text);
    }
    if let Some(text) = value.pointer("/action/message").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    if let Some(text) = value.pointer("/action/thought").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    if let Some(command) = value.pointer("/action/command").and_then(Value::as_str) {
        return Ok(command.to_owned());
    }
    if let Some(path) = value.pointer("/action/path").and_then(Value::as_str) {
        let command = value
            .pointer("/action/command")
            .and_then(Value::as_str)
            .unwrap_or("file");
        return Ok(format!("{command} {path}"));
    }
    if let Some(prompt) = value.pointer("/action/prompt").and_then(Value::as_str) {
        return Ok(prompt.to_owned());
    }
    if event_type == EventType::Notice {
        Ok(format!("OpenHands event: {entry_type}"))
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{EventRole, EventType};
    use serde_json::json;

    use super::*;

    #[test]
    fn decoder_preserves_current_and_legacy_event_semantics_exactly() {
        let current_path =
            Path::new("/profile/conversations/session/events/event-00000-current-id.json");
        let current_bytes = serde_json::to_vec(&json!({
            "id": "current-id",
            "timestamp": "2026-07-22T12:00:00.123456",
            "kind": "MessageEvent",
            "source": "agent",
            "llm_message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "current exact text"}]
            }
        }))
        .unwrap();
        let current = decode_openhands_event(current_path, &current_bytes).unwrap();
        assert_eq!(current.event_id(), "current-id");
        assert_eq!(current.event_type(), EventType::Message);
        assert_eq!(current.role(), EventRole::Assistant);
        assert_eq!(current.text(), "current exact text");
        assert_eq!(
            current.timestamp(),
            "2026-07-22T12:00:00.123456Z"
                .parse::<DateTime<Utc>>()
                .unwrap()
        );

        let legacy_path = Path::new("/profile/v1_conversations/session/0007-legacy.json");
        let legacy_bytes = serde_json::to_vec(&json!({
            "timestamp": "2026-07-22T12:00:01Z",
            "source": "agent",
            "action": {
                "kind": "ThinkAction",
                "thought": "legacy exact thought"
            }
        }))
        .unwrap();
        let legacy = decode_openhands_event(legacy_path, &legacy_bytes).unwrap();
        assert_eq!(legacy.event_id(), "0007-legacy");
        assert_eq!(legacy.event_type(), EventType::Summary);
        assert_eq!(legacy.role(), EventRole::Assistant);
        assert_eq!(legacy.text(), "legacy exact thought");
    }

    #[test]
    fn decoder_fails_closed_for_malformed_and_oversized_events() {
        let path = Path::new("/profile/v1_conversations/session/malformed.json");
        let malformed = decode_openhands_event(path, b"{not-json").unwrap_err();
        assert_eq!(malformed.kind, OpenHandsEventDecodeErrorKind::Invalid);
        assert!(malformed
            .to_string()
            .starts_with("invalid OpenHands event JSON:"));

        let missing_timestamp = decode_openhands_event(
            path,
            br#"{"id":"missing-time","kind":"MessageEvent","content":"text"}"#,
        )
        .unwrap_err();
        assert_eq!(
            missing_timestamp.to_string(),
            "OpenHands event missing-time missing valid timestamp"
        );

        let oversized = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1];
        let oversized = decode_openhands_event(path, &oversized).unwrap_err();
        assert_eq!(oversized.kind, OpenHandsEventDecodeErrorKind::TooLarge);
        assert_eq!(
            oversized.to_string(),
            format!(
                "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                MAX_PROVIDER_JSONL_LINE_BYTES + 1
            )
        );

        let ambiguous = serde_json::to_vec(&json!({
            "id": "ambiguous",
            "timestamp": "2026-07-22T12:00:01Z",
            "kind": "ObservationEvent",
            "observation": {
                "kind": "ExecuteBashObservation",
                "content": "one",
                "output": "two"
            }
        }))
        .unwrap();
        let ambiguous = decode_openhands_event(path, &ambiguous).unwrap();
        assert!(ambiguous.capture_audit().result_alias_conflict);
        assert!(ambiguous.text().contains("\"content\":\"one\""));
        assert!(ambiguous.text().contains("\"output\":\"two\""));

        let raw_duplicate = br#"{
            "id":"duplicate-key",
            "timestamp":"2026-07-22T12:00:01Z",
            "kind":"ActionEvent",
            "source":"agent",
            "tool_call_id":"call-1",
            "action":{"kind":"first_tool","kind":"second_tool","input":{"x":1}}
        }"#;
        let duplicate = decode_openhands_event(path, raw_duplicate).unwrap_err();
        assert!(duplicate
            .to_string()
            .contains("duplicate critical selector \"kind\""));

        let duplicate_payload = br#"{
            "id":"duplicate-payload",
            "timestamp":"2026-07-22T12:00:01Z",
            "kind":"ObservationEvent",
            "source":"environment",
            "observation":{"kind":"TerminalObservation","content":"one","content":"two"}
        }"#;
        let duplicate = decode_openhands_event(path, duplicate_payload).unwrap();
        assert!(duplicate.capture_audit().duplicate_key);
        assert_eq!(duplicate.text(), String::from_utf8_lossy(duplicate_payload));
    }
}
