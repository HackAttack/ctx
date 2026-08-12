use serde_json::Value;

use crate::Result;

pub(crate) use ctx_history_capture_model::fnv1a64;

pub fn compute_payload_hash(payload: &Value) -> Result<String> {
    ctx_history_core::compute_payload_hash(payload).map_err(Into::into)
}
