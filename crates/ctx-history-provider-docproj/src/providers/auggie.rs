use std::fmt;

use chrono::{DateTime, Utc};
use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserializer as _,
};
use serde_json::Value;

use crate::{CaptureError, ProviderAdapterContext, Result};
use ctx_history_capture_model::{
    exact_bounded_string_alias,
    normalization::{provider_string_field, provider_timestamp_from_fields},
    ExactJsonStringAlias,
};

const MAX_AUGGIE_LINEAGE_SESSION_ID_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AuggieLineageClaim {
    Absent,
    Exact(String),
    InvalidOrConflicting,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AuggieRawLineageAuthority {
    parent_duplicate: bool,
    root_duplicate: bool,
}

pub mod native_path;

pub(super) struct AuggieSessionData<'a> {
    pub(super) provider_session_id: String,
    pub(super) parent_session_claim: AuggieLineageClaim,
    pub(super) root_session_claim: AuggieLineageClaim,
    pub(super) chat_history: &'a [Value],
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
}

impl<'a> AuggieSessionData<'a> {
    pub(super) fn parse_with_lineage_authority(
        session: &'a Value,
        context: &ProviderAdapterContext,
        authority: AuggieRawLineageAuthority,
    ) -> Result<Self> {
        let provider_session_id = provider_string_field(session, &["sessionId", "session_id"])
            .ok_or_else(|| {
                CaptureError::InvalidPayload("Auggie session JSON is missing sessionId".to_owned())
            })?;
        let chat_history = session
            .get("chatHistory")
            .or_else(|| session.get("chat_history"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie session JSON is missing chatHistory array".to_owned(),
                )
            })?;
        let started_at = provider_timestamp_from_fields(
            session,
            &[
                "created",
                "createdAt",
                "created_at",
                "startedAt",
                "started_at",
            ],
        )
        .or_else(|| {
            chat_history
                .iter()
                .find_map(|entry| auggie_entry_time(entry, None))
        })
        .unwrap_or(context.imported_at);
        let cwd = provider_string_field(
            session,
            &[
                "workspaceRoot",
                "workspace_root",
                "workspacePath",
                "workspace_path",
                "cwd",
            ],
        );
        let parent_session_claim = if authority.parent_duplicate {
            AuggieLineageClaim::InvalidOrConflicting
        } else {
            auggie_lineage_claim(
                session,
                &[
                    "parentConversationId",
                    "parentSessionId",
                    "parent_session_id",
                ],
            )
        };
        let root_session_claim = if authority.root_duplicate {
            AuggieLineageClaim::InvalidOrConflicting
        } else {
            auggie_lineage_claim(
                session,
                &["rootConversationId", "rootSessionId", "root_session_id"],
            )
        };
        Ok(Self {
            provider_session_id,
            parent_session_claim,
            root_session_claim,
            started_at,
            cwd,
            chat_history,
        })
    }
}

pub(super) fn auggie_raw_lineage_authority(
    bytes: &[u8],
) -> serde_json::Result<AuggieRawLineageAuthority> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let authority = deserializer.deserialize_map(AuggieRawLineageVisitor)?;
    deserializer.end()?;
    Ok(authority)
}

struct AuggieRawLineageVisitor;

impl<'de> Visitor<'de> for AuggieRawLineageVisitor {
    type Value = AuggieRawLineageAuthority;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an Auggie session JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut authority = AuggieRawLineageAuthority::default();
        let mut seen = 0_u8;
        while let Some(key) = map.next_key::<String>()? {
            if let Some((bit, parent)) = auggie_lineage_alias_bit(&key) {
                if seen & bit != 0 {
                    if parent {
                        authority.parent_duplicate = true;
                    } else {
                        authority.root_duplicate = true;
                    }
                }
                seen |= bit;
            }
            map.next_value::<IgnoredAny>()?;
        }
        Ok(authority)
    }
}

fn auggie_lineage_alias_bit(key: &str) -> Option<(u8, bool)> {
    Some(match key {
        "parentConversationId" => (1 << 0, true),
        "parentSessionId" => (1 << 1, true),
        "parent_session_id" => (1 << 2, true),
        "rootConversationId" => (1 << 3, false),
        "rootSessionId" => (1 << 4, false),
        "root_session_id" => (1 << 5, false),
        _ => return None,
    })
}

fn auggie_lineage_claim(session: &Value, aliases: &[&str]) -> AuggieLineageClaim {
    let Some(object) = session.as_object() else {
        return AuggieLineageClaim::InvalidOrConflicting;
    };
    match exact_bounded_string_alias(object, aliases, MAX_AUGGIE_LINEAGE_SESSION_ID_BYTES) {
        ExactJsonStringAlias::Missing => AuggieLineageClaim::Absent,
        ExactJsonStringAlias::Exact(value) if !value.trim().is_empty() => {
            AuggieLineageClaim::Exact(value.to_owned())
        }
        ExactJsonStringAlias::Exact(_) | ExactJsonStringAlias::Ambiguous => {
            AuggieLineageClaim::InvalidOrConflicting
        }
    }
}

pub(crate) fn auggie_entry_time(entry: &Value, exchange: Option<&Value>) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        entry,
        &[
            "finishedAt",
            "finished_at",
            "createdAt",
            "created_at",
            "timestamp",
            "time",
        ],
    )
    .or_else(|| {
        exchange.and_then(|exchange| {
            provider_timestamp_from_fields(
                exchange,
                &[
                    "createdAt",
                    "created_at",
                    "updatedAt",
                    "updated_at",
                    "timestamp",
                    "time",
                ],
            )
        })
    })
}

pub(crate) fn auggie_request_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["request_message", "requestMessage"]).or_else(|| {
        auggie_nodes_text(
            exchange
                .get("request_nodes")
                .or_else(|| exchange.get("requestNodes")),
        )
    })
}

pub(crate) fn auggie_response_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["response_text", "responseText"]).or_else(|| {
        auggie_nodes_text(
            exchange
                .get("response_nodes")
                .or_else(|| exchange.get("responseNodes")),
        )
    })
}

pub(crate) fn auggie_nodes_text(value: Option<&Value>) -> Option<String> {
    let nodes = value?.as_array()?;
    let rendered = nodes
        .iter()
        .filter_map(auggie_node_text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn auggie_node_text(node: &Value) -> Option<String> {
    let object = node.as_object()?;
    match object.get("type") {
        None if object.len() == 1 => {}
        Some(kind) if object.len() == 2 && kind.as_u64() == Some(0) => {}
        _ => return None,
    }
    let text_node = match (object.get("text_node"), object.get("textNode")) {
        (Some(text_node), None) | (None, Some(text_node)) => text_node.as_object()?,
        _ => return None,
    };
    if text_node.len() != 1 {
        return None;
    }
    text_node
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests;
