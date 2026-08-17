use std::{
    io::{self, Write},
    mem::size_of,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    EventRole, EventType, LiteralFactKind, ProviderNativeSessionRelationship, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::raw_json::{audit_json, RawJsonAudit, SelectorGroup};
use super::record::{CodexDecodedRecord, CodexResultKind, CodexRetainedKind};
use crate::provider::codex::events::codex_content_text;
use crate::Result as CaptureResult;

const OWNED_ALLOCATION_OVERHEAD_BYTES: usize = 16;
pub(super) const MAX_CODEX_DURABLE_SESSION_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_DURABLE_CWD_BYTES: usize = 4 * 1024;
pub(super) const MAX_CODEX_DURABLE_METADATA_BYTES: usize = 1024;

struct JsonLengthWriter {
    bytes: usize,
}

impl Write for JsonLengthWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("encoded JSON length exceeds usize"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encoded_json_len<T>(value: &T) -> Option<usize>
where
    T: Serialize + ?Sized,
{
    let mut writer = JsonLengthWriter { bytes: 0 };
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexSessionRow {
    pub(crate) native_session_id: String,
    pub(crate) parent_native_session_id: Option<String>,
    pub(crate) root_native_session_id: Option<String>,
    pub(crate) session_relationship: Option<ProviderNativeSessionRelationship>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) role_hint: Option<String>,
    pub(crate) model_provider: Option<String>,
    pub(crate) git: Option<CodexSessionGitMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexSessionGitMetadata {
    pub(crate) commit_hash: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) repository_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexCoreRecordDraft {
    pub(crate) raw_ordinal: u64,
    pub(crate) provider_event_identity: Option<CodexProviderEventIdentityV0>,
    pub(crate) provider_event_copy: Option<CodexProviderNativeEventCopyV0>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) lexical_body: String,
    pub(crate) structured_content: Option<Value>,
    pub(crate) activity: Option<CoreActivity>,
}

impl CodexCoreRecordDraft {
    pub(crate) fn estimated_owned_bytes(&self) -> Option<usize> {
        [
            size_of::<Self>(),
            self.provider_event_identity
                .as_ref()
                .map_or(0, |identity| identity.value.capacity()),
            self.provider_event_copy.as_ref().map_or(0, |copy| {
                copy.ancestor_native_session_id
                    .capacity()
                    .saturating_add(copy.result_call_id.capacity())
            }),
            self.lexical_body.capacity(),
            self.structured_content
                .as_ref()
                .and_then(encoded_json_len)
                .unwrap_or(0),
            self.activity
                .as_ref()
                .and_then(encoded_json_len)
                .unwrap_or(0),
            self.session_cwd.as_ref().map_or(0, String::capacity),
            OWNED_ALLOCATION_OVERHEAD_BYTES.checked_mul(5)?,
        ]
        .into_iter()
        .try_fold(0_usize, usize::checked_add)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexProviderEventIdentityKindV0 {
    Id,
    CallId,
}

impl CodexProviderEventIdentityKindV0 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::CallId => "call_id",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexProviderEventIdentityV0 {
    pub(crate) kind: CodexProviderEventIdentityKindV0,
    pub(crate) value: String,
}

/// Parser-local provider-native copy candidate. This is not a Core semantic
/// classification: identity projection still has to prove the exact result
/// call identity and matching ancestor event occurrence before publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexProviderNativeEventCopyV0 {
    pub(crate) ancestor_native_session_id: String,
    pub(crate) result_call_id: String,
}

pub(super) struct CodexSourceBackedBuiltRowV0 {
    pub(super) row: CodexCoreRecordDraft,
}

pub(super) enum CodexRetainedNonMaterialized {
    ValidUnmaterializable,
    Malformed,
}

pub(super) fn build_source_backed_event_row(
    raw_ordinal: u64,
    kind: CodexRetainedKind,
    retained: &CodexDecodedRecord,
    raw_record: &[u8],
) -> CaptureResult<std::result::Result<CodexSourceBackedBuiltRowV0, CodexRetainedNonMaterialized>> {
    let audit = audit_codex_record(raw_record)?;
    let semantic = match source_backed_semantic_projection(kind, &retained.payload) {
        SourceBackedSemanticProjection::Materialized(semantic) => *semantic,
        SourceBackedSemanticProjection::ValidUnmaterializable => {
            return Ok(Err(CodexRetainedNonMaterialized::ValidUnmaterializable));
        }
        SourceBackedSemanticProjection::Malformed => {
            return Ok(Err(CodexRetainedNonMaterialized::Malformed));
        }
    };
    let lexical_body = if kind == CodexRetainedKind::ToolCall {
        serde_json::to_string(&retained.payload)?
    } else {
        semantic.lexical_body
    };
    Ok(Ok(CodexSourceBackedBuiltRowV0 {
        row: CodexCoreRecordDraft {
            raw_ordinal,
            provider_event_identity: (!audit.selector_ambiguous(SelectorGroup::CallId))
                .then(|| provider_event_identity(&retained.payload))
                .flatten(),
            provider_event_copy: None,
            occurred_at: retained.occurred_at,
            event_type: semantic.event_type,
            role: semantic.role,
            session_cwd: None,
            lexical_body,
            structured_content: (!audit.any_selector_ambiguous()).then(|| retained.payload.clone()),
            activity: codex_invocation_activity(&retained.payload, &audit, retained.occurred_at),
        },
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_source_backed_sparse_output_row(
    raw_ordinal: u64,
    provider_event_identity: Option<CodexProviderEventIdentityV0>,
    provider_event_copy: Option<CodexProviderNativeEventCopyV0>,
    provider_call_id: Option<&str>,
    occurred_at: DateTime<Utc>,
    _result_kind: CodexResultKind,
    normalized_body: String,
    structured_content: Option<Value>,
    result_content: Option<&Value>,
    raw_record: &[u8],
    payload: &Value,
    session_cwd: Option<String>,
) -> CaptureResult<Option<CodexCoreRecordDraft>> {
    let audit = audit_codex_record(raw_record)?;
    let event_type = EventType::ToolOutput;
    let lexical_body = source_backed_lexical_body(
        EventType::ToolOutput,
        Some(EventRole::Tool),
        &normalized_body,
    );
    let activity = codex_result_activity(
        provider_call_id,
        result_content,
        payload,
        &audit,
        occurred_at,
    );
    Ok(Some(CodexCoreRecordDraft {
        raw_ordinal,
        provider_event_identity: (!audit.selector_ambiguous(SelectorGroup::CallId))
            .then_some(provider_event_identity)
            .flatten(),
        provider_event_copy,
        occurred_at,
        event_type,
        role: Some(EventRole::Tool),
        session_cwd,
        lexical_body,
        structured_content: (!audit.any_selector_ambiguous())
            .then_some(structured_content)
            .flatten(),
        activity,
    }))
}

fn codex_invocation_activity(
    payload: &Value,
    audit: &RawJsonAudit,
    occurred_at: DateTime<Utc>,
) -> Option<CoreActivity> {
    let facts = audit.facts().to_vec();
    if audit.selector_ambiguous(SelectorGroup::CallId)
        || audit.selector_ambiguous(SelectorGroup::ToolName)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
    {
        return facts_only_activity(facts);
    }
    let call_id = payload
        .get("call_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)?;
    let advertised_tool = payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)?;
    let provider_call_id = TypedKey::utf8(call_id).ok()?;
    let arguments = if audit.selector_ambiguous(SelectorGroup::Arguments) {
        ActivityJsonCapture::Unavailable
    } else {
        payload
            .get("arguments")
            .or_else(|| payload.get("input"))
            .map_or(ActivityJsonCapture::Absent, |value| {
                ActivityJsonCapture::Present {
                    value: value.clone(),
                }
            })
    };
    let (protocol, server, tool) = codex_exact_tool_identity(payload, advertised_tool, audit);
    Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(provider_call_id),
        invocation: Some(ActivityInvocation {
            protocol,
            server,
            tool,
            arguments,
            started_at_unix_ms: Some(occurred_at.timestamp_millis()),
        }),
        result: None,
        facts,
    })
}

fn codex_result_activity(
    call_id: Option<&str>,
    result_content: Option<&Value>,
    payload: &Value,
    audit: &RawJsonAudit,
    occurred_at: DateTime<Utc>,
) -> Option<CoreActivity> {
    let facts = audit.facts().to_vec();
    if audit.selector_ambiguous(SelectorGroup::CallId) {
        return facts_only_activity(facts);
    }
    let call_id = call_id
        .filter(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)?;
    let provider_call_id = TypedKey::utf8(call_id).ok()?;
    let result_unavailable = audit.selector_ambiguous(SelectorGroup::Result);
    let value = (!result_unavailable)
        .then(|| result_content.cloned())
        .flatten();
    let text = if result_unavailable {
        ActivityTextCapture::Unavailable
    } else {
        match value.as_ref() {
            Some(Value::String(value)) if !value.is_empty() => ActivityTextCapture::Present {
                value: value.clone(),
            },
            Some(_) | None => ActivityTextCapture::Absent,
        }
    };
    let invocation = codex_mcp_terminal_invocation(payload, audit, occurred_at);
    Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(provider_call_id),
        invocation,
        result: Some(ActivityResult {
            status: (!audit.selector_ambiguous(SelectorGroup::Content))
                .then(|| bounded_exact_string(payload.get("status")))
                .flatten(),
            completed_at_unix_ms: Some(occurred_at.timestamp_millis()),
            duration_ns: None,
            text,
            structured_content: if result_unavailable {
                ActivityJsonCapture::Unavailable
            } else {
                value.map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present { value }
                })
            },
        }),
        facts,
    })
}

fn codex_exact_tool_identity(
    payload: &Value,
    native_tool: &str,
    audit: &RawJsonAudit,
) -> (Option<String>, Option<String>, String) {
    if !audit.selector_ambiguous(SelectorGroup::Protocol)
        && !audit.selector_ambiguous(SelectorGroup::Server)
        && !audit.selector_ambiguous(SelectorGroup::McpTool)
    {
        if let (Some("mcp"), Some(server), Some(tool)) = (
            bounded_exact_str(payload.get("protocol")),
            bounded_exact_str(payload.get("server")),
            bounded_exact_str(payload.get("tool")),
        ) {
            return (
                Some("mcp".to_owned()),
                Some(server.to_owned()),
                tool.to_owned(),
            );
        }
    }
    (None, None, native_tool.to_owned())
}

fn codex_mcp_terminal_invocation(
    payload: &Value,
    audit: &RawJsonAudit,
    occurred_at: DateTime<Utc>,
) -> Option<ActivityInvocation> {
    if audit.selector_ambiguous(SelectorGroup::Type)
        || audit.selector_ambiguous(SelectorGroup::Invocation)
        || audit.selector_ambiguous(SelectorGroup::Server)
        || audit.selector_ambiguous(SelectorGroup::McpTool)
        || payload.get("type").and_then(Value::as_str) != Some("mcp_tool_call_end")
    {
        return None;
    }
    let invocation = payload.get("invocation")?.as_object()?;
    let server = bounded_exact_str(invocation.get("server"))?;
    let tool = bounded_exact_str(invocation.get("tool"))?;
    Some(ActivityInvocation {
        protocol: Some("mcp".to_owned()),
        server: Some(server.to_owned()),
        tool: tool.to_owned(),
        arguments: if audit.selector_ambiguous(SelectorGroup::Arguments) {
            ActivityJsonCapture::Unavailable
        } else {
            invocation
                .get("arguments")
                .map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present {
                        value: value.clone(),
                    }
                })
        },
        started_at_unix_ms: Some(occurred_at.timestamp_millis()),
    })
}

fn facts_only_activity(facts: Vec<ctx_history_core::ProviderDeclaredFact>) -> Option<CoreActivity> {
    (!facts.is_empty()).then_some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts,
    })
}

fn bounded_exact_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)
}

fn bounded_exact_string(value: Option<&Value>) -> Option<String> {
    bounded_exact_str(value).map(str::to_owned)
}

pub(super) fn audit_codex_record(raw_record: &[u8]) -> serde_json::Result<RawJsonAudit> {
    audit_json(raw_record, codex_selector_group, codex_literal_kind_for_key)
}

fn codex_selector_group(key: &str) -> Option<SelectorGroup> {
    match key {
        "type" => Some(SelectorGroup::Type),
        "id" | "call_id" | "callId" => Some(SelectorGroup::CallId),
        "name" => Some(SelectorGroup::ToolName),
        "arguments" | "input" | "args" => Some(SelectorGroup::Arguments),
        "output" | "result" => Some(SelectorGroup::Result),
        "protocol" => Some(SelectorGroup::Protocol),
        "server" => Some(SelectorGroup::Server),
        "tool" => Some(SelectorGroup::McpTool),
        "content" => Some(SelectorGroup::Content),
        "status" => Some(SelectorGroup::Content),
        "invocation" => Some(SelectorGroup::Invocation),
        _ => None,
    }
}

fn codex_literal_kind_for_key(key: &str) -> Option<LiteralFactKind> {
    match key {
        "cwd" | "current_working_directory" => Some(LiteralFactKind::SessionCwd),
        "workdir" | "working_directory" => Some(LiteralFactKind::ToolWorkdir),
        "file" | "file_path" | "filepath" | "path" | "paths" | "old_path" | "new_path" => {
            Some(LiteralFactKind::File)
        }
        "url" | "uri" => Some(LiteralFactKind::Url),
        "forge" | "host" => Some(LiteralFactKind::Forge),
        "project" | "project_id" | "project_name" => Some(LiteralFactKind::Project),
        "vcs" | "repository" | "repo" | "remote" => Some(LiteralFactKind::Vcs),
        "commit" | "commit_id" | "commit_sha" | "sha" => Some(LiteralFactKind::Commit),
        "pull_request" | "pull_request_id" | "pr" | "pr_id" => Some(LiteralFactKind::PullRequest),
        "command" | "cmd" => Some(LiteralFactKind::Command),
        "branch" | "branch_name" => Some(LiteralFactKind::Branch),
        "workspace" | "workspace_id" => Some(LiteralFactKind::Workspace),
        _ => None,
    }
}

pub(super) fn provider_event_identity(payload: &Value) -> Option<CodexProviderEventIdentityV0> {
    const MAX_PROVIDER_EVENT_ID_BYTES: usize = 64 * 1024;

    [
        (CodexProviderEventIdentityKindV0::Id, "id"),
        (CodexProviderEventIdentityKindV0::CallId, "call_id"),
    ]
    .into_iter()
    .find_map(|(kind, field)| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_PROVIDER_EVENT_ID_BYTES)
            .map(|value| CodexProviderEventIdentityV0 {
                kind,
                value: value.to_owned(),
            })
    })
}

struct SourceBackedSemantic {
    event_type: EventType,
    role: Option<EventRole>,
    lexical_body: String,
}

enum SourceBackedSemanticProjection {
    Materialized(Box<SourceBackedSemantic>),
    ValidUnmaterializable,
    Malformed,
}

fn source_backed_semantic_projection(
    kind: CodexRetainedKind,
    payload: &Value,
) -> SourceBackedSemanticProjection {
    match kind {
        CodexRetainedKind::Message => source_backed_message(payload),
        CodexRetainedKind::Reasoning => source_backed_reasoning(payload),
        CodexRetainedKind::Compacted => source_backed_compacted(payload),
        CodexRetainedKind::ToolCall => source_backed_tool_call(payload),
    }
}

fn source_backed_message(payload: &Value) -> SourceBackedSemanticProjection {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let role = match role_text {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "developer" | "system" => EventRole::System,
        _ => {
            return SourceBackedSemanticProjection::Malformed;
        }
    };
    let Some(text) = payload.get("content").and_then(codex_content_text) else {
        return SourceBackedSemanticProjection::Malformed;
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Message,
        role: Some(role),
        lexical_body: source_backed_lexical_body(EventType::Message, Some(role), &text),
    }))
}

fn source_backed_reasoning(payload: &Value) -> SourceBackedSemanticProjection {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(summary) = summary else {
        return if is_encrypted_reasoning_without_plaintext(payload) {
            SourceBackedSemanticProjection::ValidUnmaterializable
        } else {
            SourceBackedSemanticProjection::Malformed
        };
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Summary,
        role: Some(EventRole::Assistant),
        lexical_body: source_backed_lexical_body(
            EventType::Summary,
            Some(EventRole::Assistant),
            &summary,
        ),
    }))
}

fn source_backed_compacted(payload: &Value) -> SourceBackedSemanticProjection {
    let Some(text) = codex_content_text(payload) else {
        return if is_source_only_compacted(payload) {
            SourceBackedSemanticProjection::ValidUnmaterializable
        } else {
            SourceBackedSemanticProjection::Malformed
        };
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Summary,
        role: Some(EventRole::System),
        lexical_body: source_backed_lexical_body(
            EventType::Summary,
            Some(EventRole::System),
            &text,
        ),
    }))
}

fn source_backed_tool_call(payload: &Value) -> SourceBackedSemanticProjection {
    let Some(text) = serde_json::to_string(payload).ok() else {
        return SourceBackedSemanticProjection::Malformed;
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        lexical_body: source_backed_lexical_body(
            EventType::ToolCall,
            Some(EventRole::Assistant),
            &text,
        ),
    }))
}

pub(super) fn source_backed_lexical_body(
    event_type: EventType,
    role: Option<EventRole>,
    text: &str,
) -> String {
    if !text.is_empty() {
        return text.to_owned();
    }
    format!(
        "{} {}",
        event_type.as_str(),
        role.map(|role| role.as_str()).unwrap_or("event")
    )
}

fn is_encrypted_reasoning_without_plaintext(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("reasoning")
        || !object
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty())
    {
        return false;
    }
    let empty_summary = match object.get("summary") {
        None | Some(Value::Null) => true,
        Some(Value::Array(parts)) => parts.is_empty(),
        _ => false,
    };
    let empty_summary_text = matches!(object.get("summary_text"), None | Some(Value::Null));
    empty_summary && empty_summary_text
}

fn is_source_only_compacted(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.get("message").is_some_and(Value::is_string)
        && object
            .get("replacement_history")
            .is_some_and(Value::is_array)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_terminal_activity_preserves_exact_server_tool_and_linkage() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let payload = serde_json::json!({
            "type": "mcp_tool_call_end",
            "call_id": "call-terminal",
            "invocation": {
                "server": "source-server",
                "tool": "source-tool",
                "arguments": {"path": "A/../B", "items": [1, 1]}
            },
            "status": "provider::failed",
            "result": {"error": "native failure"}
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();

        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8("call-terminal").unwrap())
        );
        let invocation = activity.invocation.unwrap();
        assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
        assert_eq!(invocation.server.as_deref(), Some("source-server"));
        assert_eq!(invocation.tool, "source-tool");
        assert_eq!(
            invocation.arguments,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"path": "A/../B", "items": [1, 1]})
            }
        );
        let result = activity.result.unwrap();
        assert_eq!(result.status.as_deref(), Some("provider::failed"));
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"error": "native failure"})
            }
        );
    }

    #[test]
    fn activity_preserves_exact_mcp_invocation_and_result_channels() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let invocation = serde_json::json!({
            "type": "function_call",
            "call_id": "call-exact",
            "name": "mcp__forge__open",
            "arguments": {
                "command": "  git status  ",
                "path": "./exact",
                "url": "https://example.invalid/p?q=a%20b"
            }
        });
        let raw = serde_json::to_vec(&invocation).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_invocation_activity(&invocation, &audit, occurred_at).unwrap();
        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8("call-exact").unwrap())
        );
        let invocation = activity.invocation.unwrap();
        assert_eq!(invocation.protocol, None);
        assert_eq!(invocation.server, None);
        assert_eq!(invocation.tool, "mcp__forge__open");
        assert_eq!(
            invocation.arguments,
            ActivityJsonCapture::Present {
                value: serde_json::json!({
                    "command": "  git status  ",
                    "path": "./exact",
                    "url": "https://example.invalid/p?q=a%20b"
                })
            }
        );

        let provider_result = serde_json::json!({
            "content": ["first", {"status": "failed"}],
            "command": "  git status  "
        });
        let payload = serde_json::json!({"status":"native", "result":provider_result});
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            Some("call-exact"),
            payload.get("result"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        let result = activity.result.unwrap();
        assert_eq!(result.status.as_deref(), Some("native"));
        assert_eq!(result.text, ActivityTextCapture::Absent);
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present {
                value: payload.get("result").unwrap().clone()
            }
        );
    }

    #[test]
    fn empty_result_string_is_absent_text_with_exact_structured_capture() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let payload = serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-empty",
            "output": ""
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            Some("call-empty"),
            payload.get("output"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();

        let result = activity.result.unwrap();
        assert_eq!(result.text, ActivityTextCapture::Absent);
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present {
                value: Value::String(String::new())
            }
        );
    }

    #[test]
    fn terminal_outcomes_preserve_literal_status_and_complete_result_content() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for (status, expected) in [
            (Some("provider::ok"), Some("provider::ok")),
            (Some("provider::failed"), Some("provider::failed")),
            (None, None),
        ] {
            let mut payload = serde_json::json!({
                "type": "function_call_output",
                "call_id": "call-outcome",
                "output": {"message": "complete provider result"}
            });
            if let Some(status) = status {
                payload["status"] = Value::String(status.to_owned());
            }
            let raw = serde_json::to_vec(&payload).unwrap();
            let audit = audit_codex_record(&raw).unwrap();
            let activity = codex_result_activity(
                payload.get("call_id").and_then(Value::as_str),
                payload.get("output"),
                &payload,
                &audit,
                occurred_at,
            )
            .unwrap();
            let result = activity.result.unwrap();
            assert_eq!(result.status.as_deref(), expected);
            assert_eq!(result.text, ActivityTextCapture::Absent);
            assert_eq!(
                result.structured_content,
                ActivityJsonCapture::Present {
                    value: serde_json::json!({"message": "complete provider result"})
                }
            );
        }
    }

    #[test]
    fn malformed_mcp_identity_abstains_without_losing_valid_result_activity() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for invocation in [
            serde_json::json!({"server": 7, "tool": "read", "arguments": {}}),
            serde_json::json!({"server": "server", "tool": "", "arguments": {}}),
            serde_json::json!({
                "server": "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1),
                "tool": "read",
                "arguments": {}
            }),
        ] {
            let payload = serde_json::json!({
                "type": "mcp_tool_call_end",
                "call_id": "call-malformed-identity",
                "invocation": invocation,
                "result": "valid result survives"
            });
            let raw = serde_json::to_vec(&payload).unwrap();
            let audit = audit_codex_record(&raw).unwrap();
            let activity = codex_result_activity(
                payload.get("call_id").and_then(Value::as_str),
                payload.get("result"),
                &payload,
                &audit,
                occurred_at,
            )
            .unwrap();
            assert!(activity.invocation.is_none());
            assert_eq!(
                activity.result.unwrap().text,
                ActivityTextCapture::Present {
                    value: "valid result survives".to_owned()
                }
            );
        }

        let invalid_call = serde_json::json!({
            "type": "function_call_output",
            "call_id": ["not", "a", "string"],
            "output": "unlinked result"
        });
        let raw = serde_json::to_vec(&invalid_call).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        assert!(codex_result_activity(
            invalid_call.get("call_id").and_then(Value::as_str),
            invalid_call.get("output"),
            &invalid_call,
            &audit,
            occurred_at,
        )
        .is_none());
    }

    #[test]
    fn exact_mcp_identity_boundary_is_accepted_and_max_plus_one_abstains() {
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let exact_server = "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
        let exact_tool = "t".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
        let payload = serde_json::json!({
            "type": "mcp_tool_call_end",
            "call_id": "call-boundary",
            "invocation": {
                "server": exact_server,
                "tool": exact_tool,
                "arguments": {}
            },
            "result": "boundary result"
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        let invocation = activity.invocation.unwrap();
        assert_eq!(invocation.server.as_deref(), Some(exact_server.as_str()));
        assert_eq!(invocation.tool, exact_tool);

        for component in ["server", "tool"] {
            let mut oversized = payload.clone();
            oversized["invocation"][component] =
                Value::String("x".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1));
            let raw = serde_json::to_vec(&oversized).unwrap();
            let audit = audit_codex_record(&raw).unwrap();
            let activity = codex_result_activity(
                oversized.get("call_id").and_then(Value::as_str),
                oversized.get("result"),
                &oversized,
                &audit,
                occurred_at,
            )
            .unwrap();
            assert!(activity.invocation.is_none(), "oversized {component}");
            assert!(activity.result.is_some());
        }
    }

    #[test]
    fn duplicate_selectors_withhold_linkage_and_preserve_raw_fact_order() {
        let raw = br#"{"type":"function_call","call_id":"one","call_id":"two","name":"tool","arguments":{"command":" c ","path":" p ","url":" u "}}"#;
        let payload: Value = serde_json::from_slice(raw).unwrap();
        let audit = audit_codex_record(raw).unwrap();
        let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();
        assert!(activity.provider_call_id.is_none());
        assert!(activity.invocation.is_none());
        assert_eq!(
            activity
                .facts
                .iter()
                .map(|fact| (fact.kind, fact.value.as_str()))
                .collect::<Vec<_>>(),
            [
                (LiteralFactKind::Command, " c "),
                (LiteralFactKind::File, " p "),
                (LiteralFactKind::Url, " u "),
            ]
        );
    }
}
