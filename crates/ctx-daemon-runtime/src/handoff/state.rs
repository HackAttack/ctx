use std::{
    fs,
    path::{Path, PathBuf},
    process,
    time::{Duration, SystemTime},
};

use anyhow::Result;
use ctx_history_core::utc_now;
use serde_json::{json, Map, Value};

use crate::{process_state, write_private_json_file, ProcessState};

#[derive(Debug)]
pub struct DurableHandoffFence {
    path: PathBuf,
    handoff_id: String,
    release_on_drop: bool,
}

impl DurableHandoffFence {
    pub fn armed(path: PathBuf, handoff_id: String) -> Self {
        Self {
            path,
            handoff_id,
            release_on_drop: true,
        }
    }

    pub fn handoff_id(&self) -> &str {
        &self.handoff_id
    }

    pub fn release(&self, phase: &str, helper_pid: Option<u32>) -> Result<()> {
        if read_handoff_marker_at(&self.path)
            .as_ref()
            .and_then(|value| value.get("handoff_id").and_then(Value::as_str))
            != Some(self.handoff_id.as_str())
        {
            return Ok(());
        }
        write_handoff_marker_at(&self.path, &self.handoff_id, phase, helper_pid)
    }

    pub fn complete(&mut self) -> Result<()> {
        self.release("completed", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    pub fn transfer(&mut self, helper_pid: u32) -> Result<()> {
        self.release("scheduled", Some(helper_pid))?;
        self.release_on_drop = false;
        Ok(())
    }

    pub fn abort_and_disarm(&mut self) -> Result<()> {
        self.release("aborted", None)?;
        self.release_on_drop = false;
        Ok(())
    }

    pub fn is_armed(&self) -> bool {
        self.release_on_drop
    }

    pub fn disarm(&mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for DurableHandoffFence {
    fn drop(&mut self) {
        if self.release_on_drop {
            let _ = self.release("aborted", None);
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HandoffMarkerState {
    Absent,
    Terminal,
    Active,
    CorruptOrUnreadable,
}

pub fn read_handoff_marker_at(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn handoff_marker_state_at(path: &Path, stale_after: Duration) -> HandoffMarkerState {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HandoffMarkerState::Absent;
        }
        Err(_) => return HandoffMarkerState::CorruptOrUnreadable,
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return HandoffMarkerState::CorruptOrUnreadable;
    };
    let Some(handoff_id) = value.get("handoff_id").and_then(Value::as_str) else {
        return HandoffMarkerState::CorruptOrUnreadable;
    };
    if handoff_id.is_empty() {
        return HandoffMarkerState::CorruptOrUnreadable;
    }
    let Some(phase) = value.get("phase").and_then(Value::as_str) else {
        return HandoffMarkerState::CorruptOrUnreadable;
    };
    if matches!(phase, "completed" | "aborted") {
        return HandoffMarkerState::Terminal;
    }
    let pid_key = match phase {
        "scheduled" => "helper_pid",
        "preparing" | "ready" => "owner_pid",
        _ => return HandoffMarkerState::CorruptOrUnreadable,
    };
    let Some(pid) = value
        .get(pid_key)
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
    else {
        return HandoffMarkerState::CorruptOrUnreadable;
    };
    let marker_is_fresh = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .is_some_and(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age <= stale_after)
        });
    if !marker_is_fresh {
        return HandoffMarkerState::Terminal;
    }
    match process_state(pid) {
        ProcessState::Running | ProcessState::Unknown => HandoffMarkerState::Active,
        ProcessState::NotRunning => HandoffMarkerState::Terminal,
    }
}

pub fn handoff_marker_is_active_at(path: &Path, stale_after: Duration) -> bool {
    handoff_marker_state_at(path, stale_after) == HandoffMarkerState::Active
}

pub fn process_owns_handoff_marker_at(
    path: &Path,
    token: Option<&str>,
    stale_after: Duration,
) -> bool {
    if !handoff_marker_is_active_at(path, stale_after) {
        return false;
    }
    read_handoff_marker_at(path)
        .and_then(|value| {
            value
                .get("handoff_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .as_deref()
        .is_some_and(|expected| Some(expected) == token)
}

pub fn write_handoff_marker_at(
    path: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    write_private_json_file(
        path,
        &compact_json(json!({
            "schema_version": 1,
            "handoff_id": handoff_id,
            "phase": phase,
            "owner_pid": process::id(),
            "helper_pid": helper_pid,
            "updated_at_ms": utc_now().timestamp_millis(),
        })),
    )
}

pub fn write_restart_request_at(
    root: &Path,
    opaque_restart_label: &str,
    request_id: &str,
) -> Result<PathBuf> {
    let path = root.join(format!("{request_id}.json"));
    write_private_json_file(
        &path,
        &compact_json(json!({
            "schema_version": 1,
            "request_id": request_id,
            "trigger_command": opaque_restart_label,
            "requester_pid": process::id(),
            "requested_at_ms": utc_now().timestamp_millis(),
        })),
    )?;
    Ok(path)
}

pub fn read_restart_requests_at(root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = fs::read_to_string(&path).ok()?;
            let value: Value = serde_json::from_str(&text).ok()?;
            let label = value.get("trigger_command").and_then(Value::as_str)?;
            Some((path, label.to_owned()))
        })
        .collect()
}

pub fn remove_restart_requests_at(root: &Path) {
    if let Ok(entries) = fs::read_dir(root) {
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir(root);
}

pub fn persist_handoff_before_stop(
    handoff_path: &Path,
    restart_request_root: &Path,
    handoff_id: &str,
    opaque_restart_label: Option<&str>,
    mut cooperative_stop: impl FnMut(),
) -> Result<()> {
    if let Some(label) = opaque_restart_label {
        write_restart_request_at(restart_request_root, label, handoff_id)?;
    }
    write_handoff_marker_at(handoff_path, handoff_id, "preparing", None)?;
    cooperative_stop();
    Ok(())
}

fn compact_json(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter_map(|(key, value)| {
                    let value = compact_json(value);
                    (!value.is_null()).then_some((key, value))
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(compact_json).collect()),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn restart_intent_and_fence_are_durable_before_stop() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff = temp.path().join("handoff.json");
        let restart = temp.path().join("restart");
        let observed = AtomicBool::new(false);
        persist_handoff_before_stop(&handoff, &restart, "attempt", Some("opaque-v9"), || {
            assert_eq!(
                read_handoff_marker_at(&handoff).unwrap()["phase"],
                "preparing"
            );
            assert_eq!(read_restart_requests_at(&restart)[0].1, "opaque-v9");
            observed.store(true, Ordering::SeqCst);
        })?;
        assert!(observed.load(Ordering::SeqCst));
        Ok(())
    }
}
