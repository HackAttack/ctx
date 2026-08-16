use std::{
    io::{BufReader, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

use ctx_history_core::{AgentScope, CaptureProvider, ProviderNativeSessionRelationship};
use serde_json::{json, Value};

use crate::common::io::{
    read_provider_jsonl_line_or_skip_oversized, OpenedProviderSourceFile, ProviderJsonlLineRead,
};
use crate::{CaptureError, Result, CODEX_SESSION_SOURCE_FORMAT};
use ctx_history_capture_model::time::{parse_rfc3339_utc, system_time_ms};

use crate::provider::codex::nativepath::{opened_codex_file_observation, CodexFileObservation};
use crate::provider::codex::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION};

pub(crate) const CODEX_CATALOG_MAX_SOURCES: usize = 131_072;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatalogSession {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: String,
    pub(crate) source_root: String,
    pub(crate) source_path: String,
    pub(crate) external_session_id: Option<String>,
    pub(crate) agent_type: AgentScope,
    pub(crate) role_hint: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) session_started_at_ms: Option<i64>,
    pub(crate) file_size_bytes: u64,
    pub(crate) file_modified_at_ms: i64,
    pub(crate) cataloged_at_ms: i64,
    pub(crate) metadata: Value,
}

fn hex_digest(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

pub(crate) fn ensure_catalog_source_bound(source_count: usize) -> Result<()> {
    if source_count > CODEX_CATALOG_MAX_SOURCES {
        return Err(CaptureError::InvalidPayload(format!(
            "Codex catalog contains {source_count} sources; maximum is {CODEX_CATALOG_MAX_SOURCES}"
        )));
    }
    Ok(())
}

pub(crate) fn catalog_codex_explicit_session_opened(
    path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<CatalogSession> {
    let observation = opened_codex_file_observation(path, opened.file())?;
    opened.revalidate()?;
    catalog_codex_session_opened(
        path,
        opened,
        &path.display().to_string(),
        &observation,
        system_time_ms(SystemTime::now()),
    )
}

fn catalog_codex_session_opened(
    path: &Path,
    opened: &OpenedProviderSourceFile,
    source_root: &str,
    observation: &CodexFileObservation,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let session_meta = read_codex_session_meta_from_opened(opened)?;
    let payload = session_meta.as_ref().and_then(|value| value.get("payload"));
    let source = payload
        .and_then(|payload| payload.get("source"))
        .cloned()
        .unwrap_or(Value::Null);
    let parent_thread_id = payload
        .and_then(|payload| payload.get("parent_thread_id"))
        .and_then(Value::as_str);
    let forked_from_id = payload
        .and_then(|payload| payload.get("forked_from_id"))
        .and_then(Value::as_str);
    let history_base_thread_id = payload
        .and_then(|payload| payload.pointer("/history_base/thread_id"))
        .and_then(Value::as_str);
    let external_session_id = payload
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| codex_session_id_from_path(path));
    let session_started_at_ms = payload
        .and_then(|payload| payload.get("timestamp"))
        .and_then(Value::as_str)
        .or_else(|| {
            session_meta
                .as_ref()
                .and_then(|value| value.get("timestamp"))
                .and_then(Value::as_str)
        })
        .and_then(parse_rfc3339_utc)
        .map(|timestamp| timestamp.timestamp_millis());
    let agent_type = if parent_thread_id.is_none()
        && forked_from_id.is_none()
        && history_base_thread_id.is_none()
    {
        AgentScope::Primary
    } else {
        AgentScope::Subagent
    };
    let role_hint = payload
        .and_then(|payload| payload.get("agent_role"))
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| Some(agent_type.as_str().to_owned()));

    Ok(CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: source_root.to_owned(),
        source_path: path.display().to_string(),
        external_session_id,
        agent_type,
        role_hint,
        external_agent_id: payload
            .and_then(|payload| payload.get("agent_nickname"))
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty())
            .map(str::to_owned),
        cwd: payload
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
            .map(str::to_owned),
        session_started_at_ms,
        file_size_bytes: observation.len,
        file_modified_at_ms: observation.modified_at_ms,
        cataloged_at_ms,
        metadata: json!({
            "inventory_file_change_token_v1": hex_digest(&observation.change_token),
            "inventory_file_stable_token_v1": observation.stable_token.as_ref().map(hex_digest),
            "normalization_capture_revision": CODEX_CAPTURE_REVISION,
            "normalization_policy_revision": CODEX_POLICY_REVISION,
            "originator": payload.and_then(|payload| payload.get("originator")).and_then(Value::as_str),
            "cli_version": payload.and_then(|payload| payload.get("cli_version")).and_then(Value::as_str),
            "model_provider": payload.and_then(|payload| payload.get("model_provider")).and_then(Value::as_str),
            "source_kind": codex_source_kind(&source),
            "source": source,
            "catalog_scope": "session_meta",
        }),
    })
}
fn read_codex_session_meta_from_opened(opened: &OpenedProviderSourceFile) -> Result<Option<Value>> {
    let session_meta = read_codex_session_meta(opened)?;
    opened.revalidate()?;
    Ok(session_meta)
}

fn read_codex_session_meta(opened: &OpenedProviderSourceFile) -> Result<Option<Value>> {
    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..32 {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { .. } => {}
            ProviderJsonlLineRead::Oversized { .. } => continue,
        }
        if !line.contains(&b'{') || !contains_bytes(&line, br#""session_meta""#) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Reads only the bounded session-meta prefix needed to identify a newly
/// appearing Codex membership candidate. This deliberately avoids cataloging
/// or hashing the transcript body.
pub(crate) fn probe_codex_native_session_id(
    opened: &OpenedProviderSourceFile,
) -> Result<Option<String>> {
    let first = read_codex_session_meta(opened)?
        .as_ref()
        .and_then(codex_native_session_id_from_meta);
    opened.revalidate_same_object()?;
    let second = read_codex_session_meta(opened)?
        .as_ref()
        .and_then(codex_native_session_id_from_meta);
    opened.revalidate_same_object()?;
    if first != second {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(first)
}

fn codex_native_session_id_from_meta(value: &Value) -> Option<String> {
    value
        .get("payload")
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}
pub(crate) fn codex_parent_session_id(source: &Value) -> Option<String> {
    source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn codex_session_relationship(
    source: &Value,
    parent_thread_id: Option<&str>,
    forked_from_id: Option<&str>,
    history_base_thread_id: Option<&str>,
) -> (Option<String>, Option<ProviderNativeSessionRelationship>) {
    let source_parent = codex_parent_session_id(source);
    let direct_parent = parent_thread_id
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);
    let forked_parent = forked_from_id
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);
    let history_parent = history_base_thread_id
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);
    let delegated_parent = match (source_parent, direct_parent) {
        (Some(source_parent), Some(direct_parent)) if source_parent != direct_parent => {
            return (Some(source_parent), None);
        }
        (Some(source_parent), _) => Some(source_parent),
        (None, direct_parent) => direct_parent,
    };
    if let Some(parent) = delegated_parent {
        if forked_parent
            .iter()
            .chain(history_parent.iter())
            .any(|metadata_parent| metadata_parent != &parent)
        {
            return (Some(parent), None);
        }
        return (
            Some(parent),
            Some(ProviderNativeSessionRelationship::Delegated),
        );
    }

    if let (Some(forked_parent), Some(history_parent)) = (&forked_parent, &history_parent) {
        if forked_parent != history_parent {
            return (Some(forked_parent.clone()), None);
        }
    }
    if let Some(parent) = forked_parent {
        return (
            Some(parent),
            Some(ProviderNativeSessionRelationship::Forked),
        );
    }
    if let Some(parent) = history_parent {
        return (
            Some(parent),
            Some(ProviderNativeSessionRelationship::ResumedFrom),
        );
    }
    (None, Some(ProviderNativeSessionRelationship::Root))
}
pub(crate) fn codex_source_kind(source: &Value) -> Option<String> {
    if let Some(value) = source.as_str().filter(|value| !value.trim().is_empty()) {
        return Some(value.to_owned());
    }
    if source.pointer("/subagent/thread_spawn").is_some() {
        return Some("subagent".to_owned());
    }
    if source.pointer("/thread_spawn").is_some() {
        return Some("thread_spawn".to_owned());
    }
    source
        .as_object()
        .and_then(|object| object.keys().next().cloned())
}
pub(crate) fn codex_session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 36 {
        let tail = &stem[stem.len() - 36..];
        if tail.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-') {
            return Some(tail.to_owned());
        }
    }
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}
