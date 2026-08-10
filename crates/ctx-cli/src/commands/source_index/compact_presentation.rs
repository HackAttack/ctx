use std::{collections::BTreeSet, path::Path};

use anyhow::Result;
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_refresh::verify_generation_query_authority;
use serde_json::Value;
use uuid::Uuid;

use crate::transcript::normalize_uuid_prefix;

use super::compact_ref::{
    CompactRefMap, CompactRefNamespace, CompactRefResolver, MAX_COMPACT_REF_HEX_LEN,
};

const EVENT_ID_FIELDS: &[&str] = &[
    "ctx_event_id",
    "event_id",
    "ancestor_event_id",
    "copied_from_ctx_event_id",
];
const SESSION_ID_FIELDS: &[&str] = &[
    "ctx_session_id",
    "session_id",
    "parent_ctx_session_id",
    "root_ctx_session_id",
    "claimed_root_ctx_session_id",
    "ancestor_session_id",
    "copied_from_ctx_session_id",
];
const OPTIONAL_EVENT_ID_FIELDS: &[&str] = &["ancestor_event_id", "copied_from_ctx_event_id"];
const OPTIONAL_SESSION_ID_FIELDS: &[&str] = &[
    "parent_ctx_session_id",
    "root_ctx_session_id",
    "claimed_root_ctx_session_id",
    "ancestor_session_id",
    "copied_from_ctx_session_id",
];

#[derive(Default)]
struct RenderedIds {
    events: BTreeSet<Uuid>,
    sessions: BTreeSet<Uuid>,
    optional_events: BTreeSet<Uuid>,
    optional_sessions: BTreeSet<Uuid>,
}

/// Owns the two-generation namespace used by one human-facing render pass.
///
/// The structured read model remains untouched. Callers project a clone only
/// for ANSI/plain/Markdown or MCP text after the machine result is complete.
pub(super) struct CompactPresentation<'index> {
    current: &'index VerifiedIndex,
    retained_peer: Option<VerifiedIndex>,
}

impl<'index> CompactPresentation<'index> {
    pub(super) fn open_if_needed(
        current: &'index VerifiedIndex,
        index_root: &Path,
        needed: bool,
    ) -> Result<Option<Self>> {
        needed.then(|| Self::open(current, index_root)).transpose()
    }

    pub(super) fn open(current: &'index VerifiedIndex, index_root: &Path) -> Result<Self> {
        let retained_peer =
            VerifiedIndex::open_retained_generation_peer(index_root, current.generation_id())
                .map_err(|error| match error {
                    IndexError::PinnedGenerationNotRetained { .. } => {
                        IndexError::ConcurrentGenerationChange
                    }
                    error => error,
                })?;
        if let Some(peer) = retained_peer.as_ref() {
            verify_generation_query_authority(peer).map_err(anyhow::Error::new)?;
        }
        Ok(Self {
            current,
            retained_peer,
        })
    }

    pub(super) fn resolver(&self) -> CompactRefResolver<'_> {
        CompactRefResolver::new(self.current, self.retained_peer.as_ref())
    }

    pub(super) fn project(&self, value: &Value) -> Result<Value> {
        let mut rendered_ids = RenderedIds::default();
        let mut preserved_event_ids = BTreeSet::new();
        let mut preserved_session_ids = BTreeSet::new();
        collect_unresolved_lineage_ids(
            value,
            None,
            &mut preserved_event_ids,
            &mut preserved_session_ids,
        );
        collect_rendered_ids(
            value,
            None,
            &preserved_event_ids,
            &preserved_session_ids,
            &mut rendered_ids,
        );
        let resolver = self.resolver();
        for id in &rendered_ids.optional_events {
            if !resolver.contains_exact(CompactRefNamespace::Event, *id)? {
                rendered_ids.events.remove(id);
                preserved_event_ids.insert(*id);
            }
        }
        for id in &rendered_ids.optional_sessions {
            if !resolver.contains_exact(CompactRefNamespace::Session, *id)? {
                rendered_ids.sessions.remove(id);
                preserved_session_ids.insert(*id);
            }
        }
        let references = resolver.compact_refs(
            rendered_ids.events.iter().copied(),
            rendered_ids.sessions.iter().copied(),
        )?;
        let mut projected = value.clone();
        project_rendered_ids(
            &mut projected,
            None,
            &references,
            &rendered_ids.events,
            &rendered_ids.sessions,
            &preserved_event_ids,
            &preserved_session_ids,
        )?;
        Ok(projected)
    }
}

pub(super) fn reference_needs_retained_peer(reference: &str) -> bool {
    let reference = reference.trim();
    Uuid::parse_str(reference).is_err()
        && normalize_uuid_prefix(reference, "id prefix")
            .is_ok_and(|prefix| prefix.len() <= MAX_COMPACT_REF_HEX_LEN)
}

fn collect_rendered_ids(
    value: &Value,
    field: Option<&str>,
    preserved_event_ids: &BTreeSet<Uuid>,
    preserved_session_ids: &BTreeSet<Uuid>,
    rendered_ids: &mut RenderedIds,
) {
    if let Some(id) = value.as_str().and_then(|value| Uuid::parse_str(value).ok()) {
        if field.is_some_and(|field| EVENT_ID_FIELDS.contains(&field)) {
            if field.is_some_and(|field| OPTIONAL_EVENT_ID_FIELDS.contains(&field)) {
                rendered_ids.optional_events.insert(id);
            }
            if !preserved_event_ids.contains(&id) {
                rendered_ids.events.insert(id);
            }
        } else if field.is_some_and(|field| SESSION_ID_FIELDS.contains(&field)) {
            if field.is_some_and(|field| OPTIONAL_SESSION_ID_FIELDS.contains(&field)) {
                rendered_ids.optional_sessions.insert(id);
            }
            if !preserved_session_ids.contains(&id) {
                rendered_ids.sessions.insert(id);
            }
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_rendered_ids(
                    value,
                    field,
                    preserved_event_ids,
                    preserved_session_ids,
                    rendered_ids,
                );
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                collect_rendered_ids(
                    value,
                    Some(field),
                    preserved_event_ids,
                    preserved_session_ids,
                    rendered_ids,
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn collect_unresolved_lineage_ids(
    value: &Value,
    field: Option<&str>,
    event_ids: &mut BTreeSet<Uuid>,
    session_ids: &mut BTreeSet<Uuid>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_unresolved_lineage_ids(value, field, event_ids, session_ids);
            }
        }
        Value::Object(object) => {
            if field == Some("resolution")
                && object.get("state").and_then(Value::as_str) == Some("unresolved")
            {
                if let Some(id) = object
                    .get("ctx_event_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                {
                    event_ids.insert(id);
                }
                if let Some(id) = object
                    .get("ctx_session_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                {
                    session_ids.insert(id);
                }
            }
            for (field, value) in object {
                collect_unresolved_lineage_ids(value, Some(field), event_ids, session_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn project_rendered_ids(
    value: &mut Value,
    field: Option<&str>,
    references: &CompactRefMap,
    event_ids: &BTreeSet<Uuid>,
    session_ids: &BTreeSet<Uuid>,
    unresolved_event_ids: &BTreeSet<Uuid>,
    unresolved_session_ids: &BTreeSet<Uuid>,
) -> Result<()> {
    if let Value::String(text) = value {
        if let Ok(id) = Uuid::parse_str(&*text) {
            let replacement = if field.is_some_and(|field| EVENT_ID_FIELDS.contains(&field)) {
                if unresolved_event_ids.contains(&id) {
                    None
                } else {
                    Some(references.event(id)?)
                }
            } else if field.is_some_and(|field| SESSION_ID_FIELDS.contains(&field)) {
                if unresolved_session_ids.contains(&id) {
                    None
                } else {
                    Some(references.session(id)?)
                }
            } else {
                None
            };
            if let Some(replacement) = replacement {
                *text = replacement.to_owned();
                return Ok(());
            }
        }
        if field == Some("suggested_next_commands") {
            for id in event_ids {
                *text = text.replace(&id.to_string(), references.event(*id)?);
            }
            for id in session_ids {
                *text = text.replace(&id.to_string(), references.session(*id)?);
            }
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                project_rendered_ids(
                    value,
                    field,
                    references,
                    event_ids,
                    session_ids,
                    unresolved_event_ids,
                    unresolved_session_ids,
                )?;
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                project_rendered_ids(
                    value,
                    Some(field),
                    references,
                    event_ids,
                    session_ids,
                    unresolved_event_ids,
                    unresolved_session_ids,
                )?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}
