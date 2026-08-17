use super::*;
use crate::common::json::{exact_bounded_string_alias, ExactJsonStringAlias};

pub(super) const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/openclaw/terminal-call-id/v1\0";

pub(super) type OpenClawTerminalAuthority = JsonlTerminalAuthority;

fn observe_terminal(
    authority: &mut OpenClawTerminalAuthority,
    call_id: &str,
    region: JsonlTerminalObservationRegion,
) {
    if !authority.exhausted() && !call_id.is_empty() {
        authority.observe(
            TERMINAL_CALL_ID_DOMAIN,
            call_id,
            region,
            MAX_TERMINAL_CALL_IDS,
        );
    }
}

pub(super) fn observe_terminal_record(
    authority: &mut OpenClawTerminalAuthority,
    record: &[u8],
    region: JsonlTerminalObservationRegion,
) {
    if !crate::common::json::raw_object_keys_are_unique(record) {
        authority.observe_ambiguous_terminal();
    }
    let Ok(value) = serde_json::from_slice::<Value>(record) else {
        return;
    };
    if let Some(result) = native_tool_result(&value) {
        if result.ambiguous_linkage {
            authority.observe_ambiguous_terminal();
        } else if let Some(call_id) = result.call_id {
            observe_terminal(authority, call_id, region);
        }
    }
}

pub(super) struct NativeToolCall<'a> {
    pub(super) block: &'a Value,
    pub(super) block_index: usize,
    pub(super) call_id: Option<&'a str>,
    pub(super) tool_name: Option<&'a str>,
    pub(super) command: Option<String>,
    pub(super) declared_workdir: Option<String>,
    pub(super) file_references: Vec<String>,
}

pub(super) struct NativeToolResult<'a> {
    pub(super) message: &'a Value,
    pub(super) call_id: Option<&'a str>,
    pub(super) ambiguous_linkage: bool,
    pub(super) output: &'a Value,
}

pub(super) fn native_tool_calls(value: &Value) -> Vec<NativeToolCall<'_>> {
    let message = value.get("message").unwrap_or(value);
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("toolCall"))
        .map(|(block_index, block)| native_tool_call_block(block, block_index))
        .collect()
}

fn native_tool_call_block(block: &Value, block_index: usize) -> NativeToolCall<'_> {
    let arguments = block.get("arguments").and_then(Value::as_object);
    let string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| arguments?.get(*key).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let command = string(&["command"]);
    let declared_workdir = string(&["workdir", "cwd"]);
    let tool_name = block.get("name").and_then(Value::as_str);
    let file_references = ["path", "file_path", "filePath"]
        .into_iter()
        .filter_map(|key| arguments?.get(key).and_then(Value::as_str))
        .filter(|path| !path.is_empty() && path.len() <= 16 * 1024)
        .map(str::to_owned)
        .collect();
    NativeToolCall {
        block,
        block_index,
        call_id: block.get("id").and_then(Value::as_str),
        tool_name,
        command,
        declared_workdir,
        file_references,
    }
}

pub(super) fn native_tool_result(value: &Value) -> Option<NativeToolResult<'_>> {
    let message = value.get("message").unwrap_or(value);
    let role = message.get("role").and_then(Value::as_str)?;
    if !matches!(role, "tool" | "toolResult") {
        return None;
    }
    let details = message.get("details");
    let output = details
        .or_else(|| message.get("content"))
        .unwrap_or(message);
    let call_id = message
        .as_object()
        .map_or(ExactJsonStringAlias::Missing, |object| {
            exact_bounded_string_alias(
                object,
                &["toolCallId", "tool_call_id"],
                MAX_SELECTOR_CALL_ID_BYTES,
            )
        });
    Some(NativeToolResult {
        message,
        call_id: match call_id {
            ExactJsonStringAlias::Exact(call_id) => Some(call_id),
            ExactJsonStringAlias::Missing | ExactJsonStringAlias::Ambiguous => None,
        },
        ambiguous_linkage: matches!(call_id, ExactJsonStringAlias::Ambiguous),
        output,
    })
}

pub(super) struct CompoundAdmission {
    pub(super) index: Value,
    pub(super) index_file: Option<OpenedProviderSourceFile>,
    pub(super) native_session_family: OpenClawNativeSessionFamily,
}

pub(super) fn admit_compound(
    authority: &ProviderSourceRoot,
    path: &Path,
    index_relative_path: &Path,
    transcript: Arc<OpenedProviderSourceFile>,
) -> Result<CompoundAdmission> {
    let index_file = match authority.open_file(index_relative_path) {
        Ok(index) => Some(index),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let index_bytes = index_file
        .as_ref()
        .map(|index| index.read_all_bounded(MAX_OPENCLAW_SESSION_INDEX_BYTES))
        .transpose()?;
    if let Some(index) = &index_file {
        index.revalidate()?;
    }
    let native_session_family = index_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .map(|index| native_session_family(path, &index))
        .unwrap_or(OpenClawNativeSessionFamily::Absent);
    let observation = super::super::super::OpenClawSessionObservation::from_admitted(
        path.to_path_buf(),
        transcript.metadata(),
        index_file
            .as_ref()
            .zip(index_bytes.as_deref())
            .map(|(index, bytes)| (index.metadata(), bytes)),
    )?;
    Ok(CompoundAdmission {
        index: observation.index,
        index_file,
        native_session_family,
    })
}

struct OpenClawSessionLineage {
    relationship: Option<ProviderNativeSessionRelationship>,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
}

fn resolve_session_lineage(
    agent_id: Option<&str>,
    native_session_family: &OpenClawNativeSessionFamily,
    selected_index: &Value,
) -> Result<OpenClawSessionLineage> {
    let generic_parent = related_session_claim(
        selected_index,
        agent_id,
        &["parentSessionId", "parent_session_id"],
    );
    let generic_root = related_session_claim(
        selected_index,
        agent_id,
        &["rootSessionId", "root_session_id"],
    );
    match native_session_family {
        OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id,
            root_native_session_id,
        } => {
            let contradictory = generic_parent.invalid
                || generic_root.invalid
                || generic_parent
                    .value
                    .as_ref()
                    .is_some_and(|generic| generic != parent_native_session_id)
                || generic_root
                    .value
                    .as_ref()
                    .is_some_and(|generic| generic != root_native_session_id);
            Ok(OpenClawSessionLineage {
                relationship: (!contradictory)
                    .then_some(ProviderNativeSessionRelationship::Delegated),
                parent_native_session_id: (!contradictory)
                    .then(|| parent_native_session_id.clone()),
                root_native_session_id: (!contradictory).then(|| root_native_session_id.clone()),
            })
        }
        OpenClawNativeSessionFamily::Absent | OpenClawNativeSessionFamily::Invalid => {
            let Some(parent_native_session_id) = generic_parent.value else {
                if matches!(native_session_family, OpenClawNativeSessionFamily::Invalid)
                    || generic_parent.invalid
                    || generic_root.invalid
                    || generic_root.value.is_some()
                {
                    return Err(CaptureError::InvalidPayload(
                        "OpenClaw session has invalid lineage without a resolvable parent"
                            .to_owned(),
                    ));
                }
                return Ok(OpenClawSessionLineage {
                    relationship: None,
                    parent_native_session_id: None,
                    root_native_session_id: None,
                });
            };
            let root_native_session_id = generic_root
                .value
                .unwrap_or_else(|| parent_native_session_id.clone());
            Ok(OpenClawSessionLineage {
                relationship: None,
                parent_native_session_id: Some(parent_native_session_id),
                root_native_session_id: Some(root_native_session_id),
            })
        }
    }
}

pub(super) struct SessionState {
    pub(super) provider_session_id: String,
    pub(super) agent_id: Option<String>,
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) root_session_id: Option<StableEntityId>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) agent_scope: Option<AgentScope>,
    pub(super) relationship: Option<ProviderNativeSessionRelationship>,
}

impl SessionState {
    pub(super) fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        native_session_family: &OpenClawNativeSessionFamily,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
    ) -> Result<Self> {
        let agent_id = super::super::super::openclaw_agent_id(path)
            .map(|value| super::super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let lineage = resolve_session_lineage(agent_id.as_deref(), native_session_family, index)?;
        let parent_provider_session_id = lineage.parent_native_session_id;
        let relationship = lineage.relationship;
        let agent_scope = if parent_provider_session_id.is_some() {
            Some(AgentScope::Subagent)
        } else if matches!(native_session_family, OpenClawNativeSessionFamily::Absent) {
            Some(AgentScope::Primary)
        } else {
            None
        };
        let root_provider_session_id = lineage
            .root_native_session_id
            .or_else(|| parent_provider_session_id.clone());
        let parent_session_id = parent_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?;
        let root_session_id = root_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?
            .or(parent_session_id);
        Ok(Self {
            provider_session_id,
            agent_id,
            parent_session_id,
            root_session_id,
            started_at: imported_at,
            cwd: None,
            branch: explicit_branch(index),
            agent_scope,
            relationship,
        })
    }

    pub(super) fn observe_header(&mut self, value: &Value) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.provider_session_id =
                super::super::qualify_session_id(self.agent_id.as_deref(), id);
        }
        self.started_at = provider_timestamp_value(value.get("timestamp"), self.started_at);
        self.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(super::super::capped_text);
        self.branch = self.branch.clone().or_else(|| explicit_branch(value));
    }

    pub(super) fn restore(&mut self, checkpoint: SessionCheckpoint) {
        self.provider_session_id = checkpoint.provider_session_id;
        self.started_at = checkpoint.started_at;
        self.cwd = checkpoint.cwd;
        self.branch = checkpoint.branch;
    }

    pub(super) fn checkpoint(&self) -> SessionCheckpoint {
        SessionCheckpoint {
            provider_session_id: self.provider_session_id.clone(),
            started_at: self.started_at,
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
        }
    }
}
