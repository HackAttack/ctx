use std::path::PathBuf;

use ctx_history_core::ProviderDeclaredFact;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::source::ClaudeSessionKey;

// Native event ordering reserves 16 bits for subrecords within one physical
// record, so every index from 0 through u16::MAX is representable without
// colliding with the following physical record.
pub(crate) const CLAUDE_MAX_RECORD_ROWS: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaudeEventKind {
    Message,
    Summary,
    Notice,
    ToolCall,
    ToolOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeEventIdentity {
    pub(crate) source_record_ordinal: u64,
    pub(crate) source_subrecord_index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeNativeOrder {
    pub(crate) source_record_ordinal: u64,
    pub(crate) source_subrecord_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePhysicalLocator {
    pub(crate) path: PathBuf,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) line_number: u64,
    pub(crate) record_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolCallRequest {
    pub(crate) call_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) input: Option<Value>,
    pub(crate) native_content: Value,
    pub(crate) protocol: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) explicit_tool: Option<String>,
    pub(crate) call_id_unavailable: bool,
    pub(crate) tool_name_unavailable: bool,
    pub(crate) input_unavailable: bool,
    pub(crate) mcp_identity_unavailable: bool,
    pub(crate) native_content_unavailable: bool,
    pub(crate) literal_facts: Vec<ProviderDeclaredFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeToolResult {
    pub(crate) call_id: Option<String>,
    pub(crate) native_content: Value,
    pub(crate) call_id_unavailable: bool,
    pub(crate) content_unavailable: bool,
    pub(crate) native_content_unavailable: bool,
    pub(crate) literal_facts: Vec<ProviderDeclaredFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeRetainedRow {
    pub(crate) identity: ClaudeEventIdentity,
    pub(crate) native_order: ClaudeNativeOrder,
    pub(crate) native_record_id: Option<String>,
    pub(crate) parent_native_record_id: Option<String>,
    pub(crate) kind: ClaudeEventKind,
    pub(crate) role: Option<String>,
    pub(crate) occurred_at: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) body_sha256: Option<[u8; 32]>,
    pub(crate) body_text_retention: Option<Value>,
    pub(crate) tool_call: Option<ToolCallRequest>,
    pub(crate) tool_result: Option<ClaudeToolResult>,
    pub(crate) locator: ClaudePhysicalLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeSessionMetadata {
    pub(crate) key: ClaudeSessionKey,
    pub(crate) started_at: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) git_branch: Option<String>,
}

impl ClaudeSessionMetadata {
    pub(crate) fn new(key: ClaudeSessionKey) -> Self {
        Self {
            key,
            started_at: None,
            cwd: None,
            version: None,
            git_branch: None,
        }
    }

    pub(crate) fn observe(
        &mut self,
        timestamp: Option<&str>,
        cwd: Option<&str>,
        version: Option<&str>,
        git_branch: Option<&str>,
    ) {
        if let Some(timestamp) = timestamp.filter(|value| !value.trim().is_empty()) {
            if self
                .started_at
                .as_deref()
                .is_none_or(|current| timestamp < current)
            {
                self.started_at = Some(timestamp.to_owned());
            }
        }
        self.cwd = self.cwd.clone().or_else(|| owned_nonempty(cwd));
        self.version = self.version.clone().or_else(|| owned_nonempty(version));
        self.git_branch = self
            .git_branch
            .clone()
            .or_else(|| owned_nonempty(git_branch));
    }
}

fn owned_nonempty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}
