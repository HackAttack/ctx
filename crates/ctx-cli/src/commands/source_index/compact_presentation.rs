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

#[derive(Clone, Copy)]
enum IdKind {
    Event,
    Session,
}

fn id_field(field: &str) -> Option<(IdKind, bool)> {
    Some(match field {
        "ctx_event_id" | "event_id" => (IdKind::Event, false),
        "ancestor_event_id" | "copied_from_ctx_event_id" => (IdKind::Event, true),
        "ctx_session_id" | "session_id" => (IdKind::Session, false),
        "parent_ctx_session_id"
        | "root_ctx_session_id"
        | "claimed_root_ctx_session_id"
        | "ancestor_session_id"
        | "copied_from_ctx_session_id" => (IdKind::Session, true),
        _ => return None,
    })
}

#[derive(Default)]
struct IdSet {
    rendered: BTreeSet<Uuid>,
    optional: BTreeSet<Uuid>,
    preserved: BTreeSet<Uuid>,
}

#[derive(Default)]
struct RenderedIds {
    events: IdSet,
    sessions: IdSet,
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
        collect_rendered_ids(value, None, &mut rendered_ids);
        let resolver = self.resolver();
        for (ids, namespace) in [
            (&mut rendered_ids.events, CompactRefNamespace::Event),
            (&mut rendered_ids.sessions, CompactRefNamespace::Session),
        ] {
            for id in &ids.preserved {
                ids.rendered.remove(id);
            }
            for id in &ids.optional {
                if !resolver.contains_exact(namespace, *id)? {
                    ids.rendered.remove(id);
                    ids.preserved.insert(*id);
                }
            }
        }
        let references = resolver.compact_refs(
            rendered_ids.events.rendered.iter().copied(),
            rendered_ids.sessions.rendered.iter().copied(),
        )?;
        let mut projected = value.clone();
        project_rendered_ids(&mut projected, None, &references, &rendered_ids)?;
        Ok(projected)
    }
}

pub(super) fn reference_needs_retained_peer(reference: &str) -> bool {
    let reference = reference.trim();
    Uuid::parse_str(reference).is_err()
        && normalize_uuid_prefix(reference, "id prefix")
            .is_ok_and(|prefix| prefix.len() <= MAX_COMPACT_REF_HEX_LEN)
}

fn collect_rendered_ids(value: &Value, field: Option<&str>, rendered_ids: &mut RenderedIds) {
    if let Some((id, (kind, optional))) = value
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .zip(field.and_then(id_field))
    {
        let ids = match kind {
            IdKind::Event => &mut rendered_ids.events,
            IdKind::Session => &mut rendered_ids.sessions,
        };
        ids.rendered.insert(id);
        if optional {
            ids.optional.insert(id);
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_rendered_ids(value, field, rendered_ids);
            }
        }
        Value::Object(object) => {
            if field == Some("resolution")
                && object.get("state").and_then(Value::as_str) == Some("unresolved")
            {
                for (field, ids) in [
                    ("ctx_event_id", &mut rendered_ids.events),
                    ("ctx_session_id", &mut rendered_ids.sessions),
                ] {
                    if let Some(id) = uuid_member(object, field) {
                        ids.preserved.insert(id);
                    }
                }
            }
            for (field, value) in object {
                collect_rendered_ids(value, Some(field), rendered_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn uuid_member(object: &serde_json::Map<String, Value>, field: &str) -> Option<Uuid> {
    object
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn project_rendered_ids(
    value: &mut Value,
    field: Option<&str>,
    references: &CompactRefMap,
    ids: &RenderedIds,
) -> Result<()> {
    if let Value::String(text) = value {
        if let Some((id, (kind, _))) = Uuid::parse_str(&*text).ok().zip(field.and_then(id_field)) {
            let id_set = match kind {
                IdKind::Event => &ids.events,
                IdKind::Session => &ids.sessions,
            };
            if !id_set.preserved.contains(&id) {
                *text = compact_reference(references, kind, id)?.to_owned();
            }
            return Ok(());
        }
        if field == Some("suggested_next_commands") {
            for (kind, id_set) in [
                (IdKind::Event, &ids.events),
                (IdKind::Session, &ids.sessions),
            ] {
                for id in &id_set.rendered {
                    *text =
                        text.replace(&id.to_string(), compact_reference(references, kind, *id)?);
                }
            }
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                project_rendered_ids(value, field, references, ids)?;
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                project_rendered_ids(value, Some(field), references, ids)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn compact_reference(references: &CompactRefMap, kind: IdKind, id: Uuid) -> Result<&str> {
    match kind {
        IdKind::Event => references.event(id),
        IdKind::Session => references.session(id),
    }
}
