use std::env;

use serde_json::Value;

use crate::Result;

pub(crate) use ctx_history_capture_model::fnv1a64;

pub fn compute_payload_hash(payload: &Value) -> Result<String> {
    ctx_history_core::compute_payload_hash(payload).map_err(Into::into)
}

pub(crate) fn default_machine_id() -> String {
    env::var("CTX_MACHINE_ID")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
}
