use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{CoreRecord, EventOrigin};
use serde_json::{json, Value};

pub(crate) fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}

pub fn timestamp_json(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub fn event_origin_json(origin: &EventOrigin) -> Value {
    match origin {
        EventOrigin::Unknown => json!({"kind": "unknown"}),
        EventOrigin::UniqueToSession => json!({"kind": "unique_to_session"}),
        EventOrigin::CopiedFromAncestor {
            ancestor_session_id,
            ancestor_event_id,
            proof,
        } => json!({
            "kind": "copied_from_ancestor",
            "ancestor_session_id": ancestor_session_id.as_uuid(),
            "ancestor_event_id": ancestor_event_id.as_uuid(),
            "proof": proof,
        }),
    }
}

pub(crate) fn insert_mcp_tool_call(event: &mut Value, record: &CoreRecord) {
    let Some(attribution) = record.mcp_tool_call.as_ref() else {
        return;
    };
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object.insert(
        "mcp_tool_call".to_owned(),
        json!({
            "server": attribution.server,
            "tool": attribution.tool,
        }),
    );
}
