use anyhow::{anyhow, Result};
use ctx_history_index::{
    CopiedEventLineage, CopiedEventLineagePolicy, CopiedEventLineageResolution, VerifiedIndex,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

const COPIED_LINEAGE_MAX_BYTES: usize = 64 * 1024;

#[cfg(test)]
pub(crate) fn copied_lineage_value(
    index: &VerifiedIndex,
    selected_event_id: Uuid,
    policy: CopiedEventLineagePolicy,
) -> Result<Value> {
    copied_lineage_read_model(&index.copied_event_lineage(selected_event_id, policy)?)
}

pub(crate) fn copied_lineage_read_model(lineage: &CopiedEventLineage) -> Result<Value> {
    let relationship_counts = lineage
        .relationship_counts
        .iter()
        .map(|count| {
            (
                count.session_relationship.as_str().to_owned(),
                Value::from(count.observed_count),
            )
        })
        .collect::<Map<_, _>>();
    let occurrences = lineage
        .occurrences
        .iter()
        .map(|occurrence| {
            json!({
                "ctx_event_id": occurrence.event_id.as_uuid(),
                "ctx_session_id": occurrence.session_id.as_uuid(),
                "copied_from_ctx_event_id": occurrence.copied_from_event_id.as_uuid(),
                "copied_from_ctx_session_id": occurrence.copied_from_session_id.as_uuid(),
                "parent_ctx_session_id": occurrence.parent_session_id.map(|id| id.as_uuid()),
                "claimed_root_ctx_session_id": occurrence.claimed_root_session_id.as_uuid(),
                "session_relationship": occurrence.session_relationship,
                "depth": occurrence.depth,
            })
        })
        .collect::<Vec<_>>();
    let (resolution_event, resolution_session) = match lineage.resolution {
        CopiedEventLineageResolution::Resolved {
            event_id,
            session_id,
        }
        | CopiedEventLineageResolution::Cyclic {
            event_id,
            session_id,
        } => (json!(event_id.as_uuid()), json!(session_id.as_uuid())),
        CopiedEventLineageResolution::Unresolved {
            event_id,
            session_id,
        } => (json!(event_id), json!(session_id.map(|id| id.as_uuid()))),
    };
    let value = json!({
        "schema_version": 2,
        "resolution": {
            "state": lineage.resolution.state_str(),
            "ctx_event_id": resolution_event,
            "ctx_session_id": resolution_session,
        },
        "selected_depth": lineage.selected_depth,
        "observed_count": lineage.observed_count,
        "returned": lineage.returned,
        "occurrences": occurrences,
        "relationship_counts": relationship_counts,
        "truncated": lineage.truncated,
    });
    let encoded_bytes = serde_json::to_vec(&value)?.len();
    if encoded_bytes > COPIED_LINEAGE_MAX_BYTES {
        return Err(anyhow!(
            "copied lineage for event {} requires {encoded_bytes} bytes; the maximum is {COPIED_LINEAGE_MAX_BYTES}",
            lineage.selected_event_id
        ));
    }
    Ok(value)
}

pub(crate) fn copied_lineage_summary(value: &Value) -> Option<(&Value, u64, Option<&str>, u64)> {
    let lineage = value
        .get("copied_lineage")
        .filter(|value| value.is_object())?;
    Some((
        lineage,
        lineage["observed_count"].as_u64().unwrap_or(0),
        lineage["resolution"]["state"].as_str(),
        lineage["selected_depth"].as_u64().unwrap_or(0),
    ))
}

pub(crate) fn copied_lineage_relationship_summary(lineage: &Value) -> Option<String> {
    let summary = lineage["relationship_counts"]
        .as_object()?
        .iter()
        .filter_map(|(relationship, count)| {
            count
                .as_u64()
                .filter(|count| *count != 0)
                .map(|count| format!("{relationship} {count}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    (!summary.is_empty()).then_some(summary)
}
