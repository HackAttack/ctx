use std::env;

use serde_json::Value;

#[allow(unused_imports)]
pub(super) use ctx_daemon_runtime::{
    create_private_dir_all, private_create_new_file, private_create_new_lock_file,
    private_open_existing_lock_file, secure_private_dir_permissions,
    secure_private_file_permissions,
};

pub(super) fn semantic_env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(super) fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(super) fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}
