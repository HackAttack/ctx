use std::{collections::BTreeSet, fmt};

use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_capture_model::normalization::{
    provider_normalized_result_value, provider_timestamp_value, provider_value_text,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::Result;

use super::stream::{GooseRetainedContentClass, GooseRetainedMessage};

pub(super) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f") {
        return DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc);
    }
    provider_timestamp_value(Some(&Value::String(raw.to_owned())), fallback)
}

/// Complete direct Goose result bodies in source order. Wrapper objects are
/// intentionally not searched because they are not native tool responses.
#[cfg(test)]
pub(crate) fn goose_normalized_result_content(content: &Value) -> Option<String> {
    let capture = goose_result_content_capture(content);
    (!capture.ambiguous).then_some(capture.body).flatten()
}

#[derive(Default)]
struct GooseResultContentCapture {
    body: Option<String>,
    observed: bool,
    ambiguous: bool,
}

fn goose_result_content_capture(content: &Value) -> GooseResultContentCapture {
    let mut parts = Vec::new();
    let mut observed = false;
    let mut ambiguous = false;
    visit_tool_responses(content, &mut |object| match tool_response_value(object) {
        GooseAliasedValue::Unique(value) => {
            observed = true;
            parts.push(provider_normalized_result_value(value));
        }
        GooseAliasedValue::Conflict => {
            observed = true;
            ambiguous = true;
        }
        GooseAliasedValue::Absent => {}
    });
    GooseResultContentCapture {
        body: (!parts.is_empty()).then(|| parts.join("\n")),
        observed,
        ambiguous,
    }
}

pub(crate) fn goose_message_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    collect_complete_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn visit_tool_responses(value: &Value, visitor: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_tool_responses(item, visitor);
            }
        }
        Value::Object(object)
            if object.get("type").and_then(Value::as_str) == Some("toolResponse") =>
        {
            visitor(object);
        }
        _ => {}
    }
}

fn collect_complete_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_complete_text(item, parts);
            }
        }
        Value::Object(object) => {
            let before = parts.len();
            collect_text(value, parts);
            if parts.len() == before {
                for child in object.values() {
                    collect_complete_text(child, parts);
                }
            }
        }
        _ => collect_text(value, parts),
    }
}

fn collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_text(item, parts);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("toolResponse") {
                if let GooseAliasedValue::Unique(value) = tool_response_value(object) {
                    let Some(text) = provider_value_text(value) else {
                        return;
                    };
                    parts.push(text);
                }
                return;
            }
            for key in ["text", "content", "message"] {
                if let Some(text) = object.get(key).and_then(provider_value_text) {
                    parts.push(text);
                    return;
                }
            }
        }
        _ => {
            if let Some(text) = provider_value_text(value) {
                parts.push(text);
            }
        }
    }
}

enum GooseAliasedValue<'a> {
    Absent,
    Unique(&'a Value),
    Conflict,
}

fn tool_response_value(object: &serde_json::Map<String, Value>) -> GooseAliasedValue<'_> {
    let mut selected = None;
    for key in ["toolResult", "tool_result", "result", "content", "output"] {
        let Some(candidate) = object.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return GooseAliasedValue::Conflict;
        }
        selected = Some(candidate);
    }
    selected.map_or(GooseAliasedValue::Absent, GooseAliasedValue::Unique)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GooseNativeEventKind {
    Message,
    ToolCall,
    ToolOutput,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GooseNativeEvent {
    pub(super) sqlite_rowid: i64,
    pub(super) native_order: i64,
    pub(super) native_identity: String,
    pub(super) provider_message_identity: Option<String>,
    pub(super) identity_degraded: bool,
    pub(super) session_identity: String,
    pub(super) kind: GooseNativeEventKind,
    pub(super) role: String,
    pub(super) content: Value,
    pub(super) searchable_text: String,
    pub(super) semantic_capture_ambiguous: bool,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
    pub(super) tokens_json: Option<String>,
    pub(super) metadata_json: Option<String>,
    pub(super) retained_content_bytes: u64,
    pub(super) logical_row_digest: Option<[u8; 32]>,
}

pub(super) fn normalize_goose_native_message(
    message: GooseRetainedMessage,
) -> Result<GooseNativeEvent> {
    let content: Value = serde_json::from_str(&message.content_json).map_err(|error| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose retained message {} changed classification while parsing: {error}",
            message.native_identity
        ))
    })?;
    content.as_array().ok_or_else(|| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose retained message {} is no longer an array",
            message.native_identity
        ))
    })?;
    let duplicate_key = goose_has_duplicate_json_key(&message.content_json).map_err(|error| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose retained message {} changed while auditing selectors: {error}",
            message.native_identity
        ))
    })?;
    let kind = match message.retained_class {
        GooseRetainedContentClass::Message => GooseNativeEventKind::Message,
        GooseRetainedContentClass::ToolCall => GooseNativeEventKind::ToolCall,
    };
    let searchable_text = if duplicate_key {
        message.content_json.clone()
    } else {
        goose_message_text(&content).unwrap_or_else(|| format!("Goose {} message", message.role))
    };
    Ok(GooseNativeEvent {
        sqlite_rowid: message.sqlite_rowid,
        native_order: message.native_order,
        native_identity: message.native_identity,
        provider_message_identity: message.provider_message_identity,
        identity_degraded: message.identity_degraded,
        session_identity: message.session_identity,
        kind,
        role: message.role,
        content,
        searchable_text,
        semantic_capture_ambiguous: duplicate_key,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp,
        tokens_json: message.tokens_json,
        metadata_json: message.metadata_json,
        retained_content_bytes: message.content_bytes,
        logical_row_digest: Some(message.logical_row_digest),
    })
}

pub(super) fn normalize_goose_native_output(
    message: &super::stream::GooseScannedMessage,
) -> Result<Option<GooseNativeEvent>> {
    let Some(raw_content) = message.content_json.as_deref() else {
        return Ok(None);
    };
    let content: Value = serde_json::from_str(raw_content).map_err(|error| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose selected output {} changed while building its result: {error}",
            message.native_identity
        ))
    })?;
    if !content.is_array() {
        return Ok(None);
    }
    let duplicate_key = goose_has_duplicate_json_key(raw_content).map_err(|error| {
        crate::CaptureError::InvalidPayload(format!(
            "Goose selected output {} changed while auditing selectors: {error}",
            message.native_identity
        ))
    })?;
    let result_capture = goose_result_content_capture(&content);
    if !result_capture.observed {
        return Ok(None);
    }
    let semantic_capture_ambiguous = duplicate_key || result_capture.ambiguous;
    let result_body = if semantic_capture_ambiguous {
        raw_content.to_owned()
    } else if let Some(body) = result_capture.body.filter(|body| !body.trim().is_empty()) {
        body
    } else {
        return Ok(None);
    };
    let retained_content_bytes = u64::try_from(raw_content.len()).map_err(|_| {
        crate::CaptureError::SystemInvariant("Goose diagnostic content length exceeds u64")
    })?;
    Ok(Some(GooseNativeEvent {
        sqlite_rowid: message.sqlite_rowid,
        native_order: message.native_order,
        native_identity: message.native_identity.clone(),
        provider_message_identity: message.provider_message_identity.clone(),
        identity_degraded: message.identity_degraded,
        session_identity: message.session_identity.clone(),
        kind: GooseNativeEventKind::ToolOutput,
        role: message.role.clone(),
        content,
        searchable_text: result_body,
        semantic_capture_ambiguous,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp.clone(),
        tokens_json: None,
        metadata_json: None,
        retained_content_bytes,
        logical_row_digest: message.logical_row_digest,
    }))
}

fn goose_has_duplicate_json_key(raw: &str) -> std::result::Result<bool, serde_json::Error> {
    let mut duplicate = false;
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    GooseDuplicateKeySeed(&mut duplicate).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(duplicate)
}

struct GooseDuplicateKeySeed<'a>(&'a mut bool);

impl<'de> DeserializeSeed<'de> for GooseDuplicateKeySeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(GooseDuplicateKeyVisitor(self.0))
    }
}

struct GooseDuplicateKeyVisitor<'a>(&'a mut bool);

impl<'de> Visitor<'de> for GooseDuplicateKeyVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_string<E>(self, _: String) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(GooseDuplicateKeySeed(self.0))?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                *self.0 = true;
            }
            map.next_value_seed(GooseDuplicateKeySeed(self.0))?;
        }
        Ok(())
    }
}
