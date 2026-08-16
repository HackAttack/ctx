use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::ProviderNativeEventCopy;
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

pub fn event_copy_json(copy: Option<&ProviderNativeEventCopy>) -> Option<Value> {
    copy.map(|copy| {
        json!({
            "ancestor_ctx_session_id": copy.ancestor_session_id.as_uuid(),
            "ancestor_ctx_event_id": copy.ancestor_event_id.as_uuid(),
            "proof": copy.proof,
        })
    })
}
