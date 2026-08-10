use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use serde_json::{json, Value};

use crate::compact_json;

#[cfg_attr(not(unix), allow(unused_imports))]
pub(crate) use ctx_daemon_service::DaemonQueryEndpoint;
pub(crate) use ctx_daemon_service::{
    daemon_query_request, daemon_service_endpoint_path, daemon_source_refresh_request,
    read_daemon_service_endpoint_identity, DaemonIpcService,
};

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    daemon_query_service_ping(data_root).unwrap_or(false)
}

fn daemon_query_service_ping(data_root: &Path) -> Result<bool> {
    let response = daemon_query_request(
        data_root,
        compact_json(json!({"schema_version": 1, "op": "ping"})),
        Duration::from_secs(1),
        1024,
    )?;
    Ok(response
        .as_ref()
        .and_then(|value: &Value| value.get("ok").and_then(Value::as_bool))
        == Some(true))
}

pub(crate) fn wait_for_daemon_query_service(data_root: &Path, timeout: Duration) -> bool {
    if !ctx_semantic_model::semantic_query_service_supported() {
        return false;
    }
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
