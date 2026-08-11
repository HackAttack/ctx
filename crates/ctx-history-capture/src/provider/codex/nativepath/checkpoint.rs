use std::collections::BTreeSet;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ctx_history_core::TypedKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rows::CodexSessionRow;
use crate::provider::codex::events::CodexInvocationOriginV0;

const CODEX_PENDING_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/pending-call-id/v1\0";
const MAX_CODEX_PENDING_TOOL_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;
// The shared family wraps this opaque provider payload in a typed key. Leave
// five bytes for that key's fixed tag and length envelope.
const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 256;
pub(super) const MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LegacyCodexTerminalAuthorityEntry {
    pub(super) call_id_sha256: [u8; 32],
    pub(super) candidates: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCodexTerminalAuthorityCheckpoint {
    #[serde(with = "terminal_authority_entries_wire")]
    mcp_call_ids: Vec<LegacyCodexTerminalAuthorityEntry>,
    #[serde(with = "terminal_authority_entries_wire")]
    result_call_ids: Vec<LegacyCodexTerminalAuthorityEntry>,
    mcp_exhausted: bool,
    result_exhausted: bool,
}

impl LegacyCodexTerminalAuthorityCheckpoint {
    fn validate_wire_state(&self) -> bool {
        fn entries_are_valid(entries: &[LegacyCodexTerminalAuthorityEntry]) -> bool {
            entries.len() <= MAX_CODEX_MCP_TERMINAL_AUTHORITIES
                && entries
                    .iter()
                    .all(|entry| matches!(entry.candidates, 1 | 2))
                && entries
                    .windows(2)
                    .all(|entries| entries[0].call_id_sha256 < entries[1].call_id_sha256)
        }

        entries_are_valid(&self.mcp_call_ids)
            && entries_are_valid(&self.result_call_ids)
            && (!self.mcp_exhausted || self.mcp_call_ids.is_empty())
            && (!self.result_exhausted || self.result_call_ids.is_empty())
    }
}

mod terminal_authority_entries_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    const ENTRY_BYTES: usize = 33;

    pub(super) fn serialize<S>(
        entries: &[LegacyCodexTerminalAuthorityEntry],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut packed = Vec::with_capacity(entries.len().saturating_mul(ENTRY_BYTES));
        for entry in entries {
            packed.push(entry.candidates);
            packed.extend_from_slice(&entry.call_id_sha256);
        }
        serializer.serialize_str(&BASE64_STANDARD.encode(packed))
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Vec<LegacyCodexTerminalAuthorityEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let packed = BASE64_STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if packed.len() % ENTRY_BYTES != 0 {
            return Err(D::Error::custom(
                "Codex terminal authority has an incomplete packed entry",
            ));
        }
        packed
            .chunks_exact(ENTRY_BYTES)
            .map(|entry| {
                Ok(LegacyCodexTerminalAuthorityEntry {
                    call_id_sha256: entry[1..].try_into().map_err(D::Error::custom)?,
                    candidates: entry[0],
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LegacyCodexRepositoryCandidateAuthorityEntry {
    pub(super) call_id_sha256: [u8; 32],
    pub(super) calls: u8,
    pub(super) results: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCodexRepositoryCandidateAuthorityCheckpoint {
    #[serde(with = "repository_candidate_authority_entries_wire")]
    entries: Vec<LegacyCodexRepositoryCandidateAuthorityEntry>,
    exhausted: bool,
}

impl LegacyCodexRepositoryCandidateAuthorityCheckpoint {
    fn validate_wire_state(&self) -> bool {
        self.entries.len() <= MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES
            && self
                .entries
                .iter()
                .all(|entry| matches!(entry.calls, 1 | 2) && entry.results <= 2)
            && self
                .entries
                .windows(2)
                .all(|entries| entries[0].call_id_sha256 < entries[1].call_id_sha256)
            && (!self.exhausted || self.entries.is_empty())
    }
}

mod repository_candidate_authority_entries_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    const ENTRY_BYTES: usize = 34;

    pub(super) fn serialize<S>(
        entries: &[LegacyCodexRepositoryCandidateAuthorityEntry],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut packed = Vec::with_capacity(entries.len().saturating_mul(ENTRY_BYTES));
        for entry in entries {
            packed.push(entry.calls);
            packed.push(entry.results);
            packed.extend_from_slice(&entry.call_id_sha256);
        }
        serializer.serialize_str(&BASE64_STANDARD.encode(packed))
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Vec<LegacyCodexRepositoryCandidateAuthorityEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let packed = BASE64_STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if packed.len() % ENTRY_BYTES != 0 {
            return Err(D::Error::custom(
                "Codex repository candidate authority has an incomplete packed entry",
            ));
        }
        packed
            .chunks_exact(ENTRY_BYTES)
            .map(|entry| {
                Ok(LegacyCodexRepositoryCandidateAuthorityEntry {
                    call_id_sha256: entry[2..].try_into().map_err(D::Error::custom)?,
                    calls: entry[0],
                    results: entry[1],
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexPendingToolAuthority {
    #[serde(with = "sha256_wire")]
    call_id_sha256: [u8; 32],
    pub(super) record_start: u64,
    pub(super) record_end: u64,
    pub(super) raw_ordinal: u64,
    continuation_cell_id: Option<String>,
    continuation_conflicted: bool,
    #[serde(with = "sha256_vec_wire")]
    continuation_call_id_sha256: Vec<[u8; 32]>,
    continuation_capacity_exceeded: bool,
    correlation_ambiguous: bool,
    invocation_origin: CodexInvocationOriginV0,
}

mod sha256_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    pub(super) fn serialize<S>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(digest))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        BASE64_STANDARD
            .decode(encoded)
            .map_err(D::Error::custom)?
            .try_into()
            .map_err(|bytes: Vec<u8>| {
                D::Error::custom(format!(
                    "Codex pending authority digest has {} bytes, expected 32",
                    bytes.len()
                ))
            })
    }
}

mod sha256_vec_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    pub(super) fn serialize<S>(digests: &[[u8; 32]], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut packed = Vec::with_capacity(digests.len().saturating_mul(32));
        for digest in digests {
            packed.extend_from_slice(digest);
        }
        serializer.serialize_str(&BASE64_STANDARD.encode(packed))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let packed = BASE64_STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if packed.len() % 32 != 0 {
            return Err(D::Error::custom(
                "Codex pending authority has an incomplete packed digest",
            ));
        }
        packed
            .chunks_exact(32)
            .map(|digest| digest.try_into().map_err(D::Error::custom))
            .collect()
    }
}

impl CodexPendingToolAuthority {
    pub(super) fn new(
        call_id: &str,
        record_start: u64,
        record_end: u64,
        raw_ordinal: u64,
        invocation_origin: CodexInvocationOriginV0,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(CODEX_PENDING_CALL_ID_DOMAIN);
        hasher.update(call_id.as_bytes());
        Self {
            call_id_sha256: hasher.finalize().into(),
            record_start,
            record_end,
            raw_ordinal,
            continuation_cell_id: None,
            continuation_conflicted: false,
            continuation_call_id_sha256: Vec::new(),
            continuation_capacity_exceeded: false,
            correlation_ambiguous: false,
            invocation_origin,
        }
    }

    pub(super) fn matches_call_id(&self, call_id: &str) -> bool {
        Self::new(
            call_id,
            self.record_start,
            self.record_end,
            self.raw_ordinal,
            CodexInvocationOriginV0::Unproven,
        )
        .call_id_sha256
            == self.call_id_sha256
    }

    pub(super) fn assign_continuation(&mut self, cell_id: &str) -> bool {
        if cell_id.is_empty()
            || cell_id.len() > MAX_CODEX_CONTINUATION_CELL_ID_BYTES
            || !cell_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
            || self
                .continuation_cell_id
                .as_deref()
                .is_some_and(|existing| existing != cell_id)
        {
            return false;
        }
        self.continuation_cell_id = Some(cell_id.to_owned());
        self.continuation_conflicted = false;
        true
    }

    pub(super) fn mark_continuation_conflict(&mut self, cell_id: &str) -> bool {
        if !self.assign_continuation(cell_id) {
            return false;
        }
        self.continuation_conflicted = true;
        true
    }

    pub(super) fn clear_continuation(&mut self) {
        self.continuation_cell_id = None;
        self.continuation_conflicted = false;
        self.continuation_call_id_sha256.clear();
        self.continuation_capacity_exceeded = false;
    }

    pub(super) fn continuation_cell_id(&self) -> Option<&str> {
        self.continuation_cell_id.as_deref()
    }

    pub(super) fn continuation_conflicted(&self) -> bool {
        self.continuation_conflicted
    }

    pub(super) fn record_continuation_call(&mut self, digest: [u8; 32]) {
        if digest == [0; 32] || self.continuation_call_id_sha256.contains(&digest) {
            return;
        }
        if self.continuation_call_id_sha256.len() >= MAX_CODEX_TOOL_CONTEXTS {
            self.continuation_capacity_exceeded = true;
        } else {
            self.continuation_call_id_sha256.push(digest);
        }
    }

    pub(super) fn continuation_call_id_sha256(&self) -> &[[u8; 32]] {
        &self.continuation_call_id_sha256
    }

    pub(super) fn continuation_capacity_exceeded(&self) -> bool {
        self.continuation_capacity_exceeded
    }

    pub(super) fn mark_correlation_ambiguous(&mut self) {
        self.correlation_ambiguous = true;
    }

    pub(super) fn correlation_ambiguous(&self) -> bool {
        self.correlation_ambiguous
    }

    pub(super) fn invocation_origin(&self) -> &CodexInvocationOriginV0 {
        &self.invocation_origin
    }

    pub(super) fn set_invocation_origin(&mut self, origin: CodexInvocationOriginV0) {
        self.invocation_origin = origin;
    }
}

const LEGACY_CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 1;
const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCodexSemanticCheckpointV1 {
    version: u8,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    terminal_authority: LegacyCodexTerminalAuthorityCheckpoint,
    repository_candidate_authority: LegacyCodexRepositoryCandidateAuthorityCheckpoint,
    owner: CodexSessionRow,
    local_turn_started: bool,
}

/// Provider-only continuation state. Physical position, framing, digests,
/// observations, and lifecycle evidence live exclusively in the enclosing
/// shared JSONL family checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    owner: CodexSessionRow,
    local_turn_started: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CodexSemanticCheckpointWire {
    LegacyV1(LegacyCodexSemanticCheckpointV1),
    Current(CodexSemanticCheckpoint),
}

impl CodexSemanticCheckpoint {
    pub(super) fn new(
        pending_tool_authorities: &[CodexPendingToolAuthority],
        owner: CodexSessionRow,
        local_turn_started: bool,
    ) -> serde_json::Result<Self> {
        let mut checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            pending_tool_authorities: pending_tool_authorities.to_vec(),
            owner,
            local_turn_started,
        };
        // The maximum owner plus 24 maximum pending authorities still exceeds
        // the nested key contract. Twenty fit here, but the worst 7,168-byte
        // path makes that full-family envelope 74,519 bytes (8,983 over the
        // 65,536-byte maximum); outer shedding retains 16 at 64,939 bytes.
        loop {
            let encoded = serde_json::to_vec(&checkpoint)?;
            if encoded.len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
                return Ok(checkpoint);
            }
            if checkpoint.pending_tool_authorities.pop().is_none() {
                return Err(checkpoint_size_error(encoded.len()));
            }
        }
    }

    #[cfg(test)]
    pub(super) fn encode(&self) -> serde_json::Result<Vec<u8>> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        Ok(encoded)
    }

    pub(super) fn encode_key(&self) -> serde_json::Result<TypedKey> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        TypedKey::utf8(encoded)
            .map_err(|error| <serde_json::Error as serde::ser::Error>::custom(error.to_string()))
    }

    pub(super) fn decode(bytes: &[u8]) -> serde_json::Result<Self> {
        if bytes.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(bytes.len()));
        }
        match serde_json::from_slice::<CodexSemanticCheckpointWire>(bytes)? {
            CodexSemanticCheckpointWire::LegacyV1(checkpoint) => checkpoint.into_current(),
            CodexSemanticCheckpointWire::Current(checkpoint) => {
                checkpoint.validate_wire_state()?;
                Ok(checkpoint)
            }
        }
    }

    pub(super) fn decode_key(key: &TypedKey) -> serde_json::Result<Self> {
        match key {
            // Candidate checkpoints used Bytes. The compact representation is
            // the same versioned JSON carried directly as UTF-8.
            TypedKey::Bytes(bytes) => Self::decode(bytes),
            TypedKey::Utf8(json) => Self::decode(json.as_bytes()),
            _ => Err(<serde_json::Error as serde::de::Error>::custom(
                "Codex semantic checkpoint has an invalid key type",
            )),
        }
    }

    pub(super) fn shed_optional_pending_evidence_key(
        key: &TypedKey,
    ) -> serde_json::Result<Option<TypedKey>> {
        let mut checkpoint = Self::decode_key(key)?;
        if checkpoint.pending_tool_authorities.pop().is_none() {
            return Ok(None);
        }
        checkpoint.encode_key().map(Some)
    }

    pub(super) fn pending_tool_authorities(&self) -> &[CodexPendingToolAuthority] {
        &self.pending_tool_authorities
    }

    pub(super) fn owner(&self) -> &CodexSessionRow {
        &self.owner
    }

    pub(super) fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !pending_tool_authorities_are_valid(
                &self.pending_tool_authorities,
                &self.owner,
                self.local_turn_started,
            )
        {
            return Err(serde::de::Error::custom(
                "invalid Codex semantic checkpoint state",
            ));
        }
        Ok(())
    }
}

impl LegacyCodexSemanticCheckpointV1 {
    fn into_current(self) -> serde_json::Result<CodexSemanticCheckpoint> {
        if self.version != LEGACY_CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !self.terminal_authority.validate_wire_state()
            || !self.repository_candidate_authority.validate_wire_state()
            || !pending_tool_authorities_are_valid(
                &self.pending_tool_authorities,
                &self.owner,
                self.local_turn_started,
            )
        {
            return Err(serde::de::Error::custom(
                "invalid legacy Codex semantic checkpoint state",
            ));
        }
        Ok(CodexSemanticCheckpoint {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            pending_tool_authorities: self.pending_tool_authorities,
            owner: self.owner,
            local_turn_started: self.local_turn_started,
        })
    }
}

fn pending_tool_authorities_are_valid(
    authorities: &[CodexPendingToolAuthority],
    owner: &CodexSessionRow,
    local_turn_started: bool,
) -> bool {
    let mut call_ids = BTreeSet::new();
    let mut record_spans = BTreeSet::new();
    let mut raw_ordinals = BTreeSet::new();
    let mut continuation_cells = BTreeSet::new();
    authorities.len() <= MAX_CODEX_TOOL_CONTEXTS
        && authorities.iter().all(|authority| {
            !(matches!(
                owner.session_relationship,
                ctx_history_core::SessionRelationshipKind::Forked
                    | ctx_history_core::SessionRelationshipKind::ResumedFrom
            ) && !local_turn_started
                && matches!(
                    authority.invocation_origin(),
                    CodexInvocationOriginV0::UniqueToSession
                ))
                && authority.record_start < authority.record_end
                && authority
                    .record_end
                    .checked_sub(authority.record_start)
                    .is_some_and(|len| len <= MAX_CODEX_PENDING_TOOL_RECORD_BYTES)
                && authority.call_id_sha256 != [0; 32]
                && call_ids.insert(authority.call_id_sha256)
                && record_spans.insert((authority.record_start, authority.record_end))
                && raw_ordinals.insert(authority.raw_ordinal)
                && authority
                    .continuation_cell_id
                    .as_ref()
                    .is_none_or(|cell_id| {
                        !cell_id.is_empty()
                            && cell_id.len() <= MAX_CODEX_CONTINUATION_CELL_ID_BYTES
                            && cell_id.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric()
                                    || matches!(byte, b'-' | b'_' | b'.' | b':')
                            })
                            && continuation_cells.insert(cell_id.clone())
                    })
                && authority.continuation_call_id_sha256.len() <= MAX_CODEX_TOOL_CONTEXTS
                && (!authority.continuation_capacity_exceeded
                    || authority.continuation_call_id_sha256.len() == MAX_CODEX_TOOL_CONTEXTS)
                && (!authority.continuation_conflicted || authority.continuation_cell_id.is_some())
                && !authority.continuation_call_id_sha256.contains(&[0; 32])
                && authority
                    .continuation_call_id_sha256
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    == authority.continuation_call_id_sha256.len()
        })
}

fn checkpoint_size_error(actual: usize) -> serde_json::Error {
    <serde_json::Error as serde::ser::Error>::custom(format!(
        "Codex semantic checkpoint payload has {actual} bytes, maximum is {MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES}"
    ))
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use ctx_history_core::SessionRelationshipKind;
    use serde_json::json;

    use super::*;
    use crate::provider::codex::nativepath::rows::{
        CodexSessionGitMetadata, MAX_CODEX_DURABLE_CWD_BYTES, MAX_CODEX_DURABLE_METADATA_BYTES,
        MAX_CODEX_DURABLE_SESSION_ID_BYTES,
    };

    fn owner() -> CodexSessionRow {
        CodexSessionRow {
            native_session_id: "checkpoint-owner".to_owned(),
            parent_native_session_id: None,
            advisory_session_id: None,
            root_native_session_id: Some("checkpoint-owner".to_owned()),
            session_relationship: SessionRelationshipKind::Root,
            started_at: DateTime::parse_from_rfc3339("2026-08-11T00:00:00Z")
                .unwrap()
                .to_utc(),
            cwd: Some("/workspace".to_owned()),
            originator: Some("codex_cli_rs".to_owned()),
            cli_version: Some("0.1.0".to_owned()),
            source_kind: Some("cli".to_owned()),
            external_agent_id: None,
            role_hint: Some("primary".to_owned()),
            model_provider: Some("openai".to_owned()),
            git: None,
        }
    }

    fn maximal_owner() -> CodexSessionRow {
        let session_id = "s".repeat(MAX_CODEX_DURABLE_SESSION_ID_BYTES);
        let metadata = "m".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
        CodexSessionRow {
            native_session_id: session_id.clone(),
            parent_native_session_id: Some(session_id.clone()),
            advisory_session_id: Some(session_id.clone()),
            root_native_session_id: Some(session_id),
            session_relationship: SessionRelationshipKind::Delegated,
            started_at: DateTime::parse_from_rfc3339("9999-12-31T23:59:59.999999999Z")
                .unwrap()
                .to_utc(),
            cwd: Some("c".repeat(MAX_CODEX_DURABLE_CWD_BYTES)),
            originator: Some(metadata.clone()),
            cli_version: Some(metadata.clone()),
            source_kind: Some(metadata.clone()),
            external_agent_id: Some(metadata.clone()),
            role_hint: Some(metadata.clone()),
            model_provider: Some(metadata.clone()),
            git: Some(CodexSessionGitMetadata {
                commit_hash: Some(metadata.clone()),
                branch: Some(metadata.clone()),
                repository_url: Some(metadata),
            }),
        }
    }

    fn terminal_entries(entries: usize) -> Vec<LegacyCodexTerminalAuthorityEntry> {
        (0..entries)
            .map(|index| LegacyCodexTerminalAuthorityEntry {
                call_id_sha256: [u8::try_from(index).unwrap(); 32],
                candidates: 1,
            })
            .collect()
    }

    fn repository_entries(entries: usize) -> Vec<LegacyCodexRepositoryCandidateAuthorityEntry> {
        (0..entries)
            .map(|index| LegacyCodexRepositoryCandidateAuthorityEntry {
                call_id_sha256: [u8::try_from(index).unwrap(); 32],
                calls: 1,
                results: 1,
            })
            .collect()
    }

    fn checkpoint(call_id: &str) -> CodexSemanticCheckpoint {
        CodexSemanticCheckpoint::new(
            &[CodexPendingToolAuthority::new(
                call_id,
                10,
                20,
                1,
                CodexInvocationOriginV0::UniqueToSession,
            )],
            owner(),
            true,
        )
        .unwrap()
    }

    fn maximal_pending_authorities() -> Vec<CodexPendingToolAuthority> {
        (0..MAX_CODEX_TOOL_CONTEXTS)
            .map(|index| {
                let mut authority = CodexPendingToolAuthority::new(
                    &format!("pending-{index}"),
                    u64::try_from(index * 2 + 1).unwrap(),
                    u64::try_from(index * 2 + 2).unwrap(),
                    u64::try_from(index).unwrap(),
                    CodexInvocationOriginV0::Unproven,
                );
                authority.continuation_cell_id = Some(format!(
                    "cell-{index:02}-{}",
                    "x".repeat(MAX_CODEX_CONTINUATION_CELL_ID_BYTES - 8)
                ));
                authority.continuation_call_id_sha256 = (0..MAX_CODEX_TOOL_CONTEXTS)
                    .map(|digest| {
                        let mut value = [u8::try_from(digest + 1).unwrap(); 32];
                        value[0] = u8::try_from(index + 1).unwrap();
                        value
                    })
                    .collect();
                authority
            })
            .collect()
    }

    fn legacy_checkpoint(
        pending_tool_authorities: Vec<CodexPendingToolAuthority>,
        owner: CodexSessionRow,
    ) -> LegacyCodexSemanticCheckpointV1 {
        LegacyCodexSemanticCheckpointV1 {
            version: LEGACY_CODEX_SEMANTIC_CHECKPOINT_VERSION,
            pending_tool_authorities,
            terminal_authority: LegacyCodexTerminalAuthorityCheckpoint {
                mcp_call_ids: terminal_entries(MAX_CODEX_MCP_TERMINAL_AUTHORITIES),
                result_call_ids: terminal_entries(MAX_CODEX_MCP_TERMINAL_AUTHORITIES),
                mcp_exhausted: false,
                result_exhausted: false,
            },
            repository_candidate_authority: LegacyCodexRepositoryCandidateAuthorityCheckpoint {
                entries: repository_entries(MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES),
                exhausted: false,
            },
            owner,
            local_turn_started: true,
        }
    }

    #[test]
    fn semantic_codec_round_trips_both_key_generations_without_event_bodies() {
        let call_id = "checkpoint-call-id-must-be-digested";
        let secret = "event-body-secret-must-not-survive";
        let checkpoint = checkpoint(call_id);

        let bytes = checkpoint.encode().unwrap();
        assert_eq!(CodexSemanticCheckpoint::decode(&bytes).unwrap(), checkpoint);
        assert!(!String::from_utf8_lossy(&bytes).contains(call_id));
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));

        let compact = checkpoint.encode_key().unwrap();
        assert!(matches!(compact, TypedKey::Utf8(_)));
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&compact).unwrap(),
            checkpoint
        );
        let legacy = TypedKey::bytes(bytes).unwrap();
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&legacy).unwrap(),
            checkpoint
        );
    }

    #[test]
    fn legacy_checkpoint_reads_and_reencodes_without_authority_snapshots() {
        let pending = checkpoint("legacy-checkpoint-call").pending_tool_authorities;
        let legacy = legacy_checkpoint(pending.clone(), owner());
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        let migrated = CodexSemanticCheckpoint::decode(&legacy_bytes).unwrap();
        assert_eq!(migrated.pending_tool_authorities, pending);

        let encoded = migrated.encode().unwrap();
        let wire = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
        assert_eq!(wire["version"], CODEX_SEMANTIC_CHECKPOINT_VERSION);
        assert!(wire.get("terminal_authority").is_none());
        assert!(wire.get("repository_candidate_authority").is_none());
        assert!(encoded.len() < legacy_bytes.len());
    }

    #[test]
    fn semantic_codec_rejects_malformed_unknown_version_duplicate_and_oversize() {
        let checkpoint = checkpoint("codec-rejection-call");
        assert!(CodexSemanticCheckpoint::decode(b"{").is_err());
        assert!(CodexSemanticCheckpoint::decode_key(&TypedKey::U64(1)).is_err());

        let mut unknown = serde_json::to_value(&checkpoint).unwrap();
        unknown["unknown"] = json!(true);
        assert!(CodexSemanticCheckpoint::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let mut version = serde_json::to_value(&checkpoint).unwrap();
        version["version"] = json!(CODEX_SEMANTIC_CHECKPOINT_VERSION + 1);
        assert!(CodexSemanticCheckpoint::decode(&serde_json::to_vec(&version).unwrap()).is_err());

        let mut malformed_digest = serde_json::to_value(&checkpoint).unwrap();
        malformed_digest["pending_tool_authorities"][0]["call_id_sha256"] = json!("not-base64");
        assert!(
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&malformed_digest).unwrap())
                .is_err()
        );

        let mut duplicate_pending = serde_json::to_value(&checkpoint).unwrap();
        let duplicate = duplicate_pending["pending_tool_authorities"][0].clone();
        duplicate_pending["pending_tool_authorities"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&duplicate_pending).unwrap())
                .is_err()
        );

        let mut duplicate_terminal = legacy_checkpoint(Vec::new(), owner());
        duplicate_terminal
            .terminal_authority
            .mcp_call_ids
            .push(duplicate_terminal.terminal_authority.mcp_call_ids[0]);
        assert!(
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&duplicate_terminal).unwrap())
                .is_err()
        );

        assert!(CodexSemanticCheckpoint::decode(&vec![
            b' ';
            MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES + 1
        ])
        .is_err());
        let mut oversized = checkpoint;
        oversized.owner.cwd = Some("x".repeat(MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES));
        assert!(oversized.encode().is_err());
        assert!(oversized.encode_key().is_err());
    }

    #[test]
    fn maximal_semantic_payload_sheds_only_optional_pending_evidence() {
        let pending = maximal_pending_authorities();
        let owner = maximal_owner();
        let unchecked = CodexSemanticCheckpoint {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            pending_tool_authorities: pending.clone(),
            owner: owner.clone(),
            local_turn_started: true,
        };
        assert!(
            serde_json::to_vec(&unchecked).unwrap().len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES
        );

        let checkpoint = CodexSemanticCheckpoint::new(&pending, owner.clone(), true).unwrap();
        assert!(checkpoint.pending_tool_authorities.len() < pending.len());
        assert_eq!(
            checkpoint.pending_tool_authorities,
            pending[..checkpoint.pending_tool_authorities.len()]
        );
        assert_eq!(checkpoint.owner, owner);
        assert!(checkpoint.encode().unwrap().len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES);
    }

    #[test]
    fn maximal_semantics_fit_the_full_family_frontier_with_exact_legacy_delta() {
        let pending = maximal_pending_authorities();
        let owner = maximal_owner();

        let mut legacy = legacy_checkpoint(pending.clone(), owner.clone());
        while serde_json::to_vec(&legacy).unwrap().len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            legacy.pending_tool_authorities.pop().unwrap();
        }
        assert_eq!(legacy.pending_tool_authorities.len(), 5);
        let legacy_semantic_bytes = serde_json::to_string(&legacy).unwrap().len();
        let legacy_key = TypedKey::utf8(serde_json::to_string(&legacy).unwrap()).unwrap();
        let (legacy_family_bytes, legacy_fits) = crate::provider::source_backed::family::jsonl::full_family_checkpoint_frontier_contract_for_test(
            legacy_key,
            crate::provider::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES,
        )
        .unwrap();
        assert_eq!(legacy_family_bytes, 72_932);
        assert!(!legacy_fits);

        let migrated =
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        let migrated_semantic_bytes = migrated.encode().unwrap().len();
        let (migrated_family_bytes, migrated_fits) = crate::provider::source_backed::family::jsonl::full_family_checkpoint_frontier_contract_for_test(
            migrated.encode_key().unwrap(),
            crate::provider::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES,
        )
        .unwrap();
        assert_eq!(legacy_semantic_bytes, 63_969);
        assert_eq!(migrated_semantic_bytes, 29_658);
        assert_eq!(migrated_family_bytes, 38_599);
        assert!(migrated_fits);

        let checkpoint = CodexSemanticCheckpoint::new(&pending, owner.clone(), true).unwrap();
        let mut key = checkpoint.encode_key().unwrap();
        let initial_pending = checkpoint.pending_tool_authorities.len();
        let (before_bytes, before_fits) = crate::provider::source_backed::family::jsonl::full_family_checkpoint_frontier_contract_for_test(
            key.clone(),
            crate::provider::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES,
        )
        .unwrap();
        assert!(!before_fits);

        let (after_bytes, after_fits) = loop {
            let Some(smaller) =
                CodexSemanticCheckpoint::shed_optional_pending_evidence_key(&key).unwrap()
            else {
                panic!("mandatory Codex authority did not fit the full family frontier");
            };
            key = smaller;
            let envelope = crate::provider::source_backed::family::jsonl::full_family_checkpoint_frontier_contract_for_test(
                key.clone(),
                crate::provider::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES,
            )
            .unwrap();
            if envelope.1 {
                break envelope;
            }
        };
        let fitted = CodexSemanticCheckpoint::decode_key(&key).unwrap();
        assert_eq!(before_bytes, 74_519);
        assert_eq!(after_bytes, 64_939);
        assert_eq!(initial_pending, 20);
        assert_eq!(fitted.pending_tool_authorities.len(), 16);
        assert!(after_fits);
        assert!(after_bytes <= 64 * 1024);
        assert!(fitted.pending_tool_authorities.len() < initial_pending);
        assert_eq!(fitted.owner, owner);
        eprintln!(
            "maximal Codex checkpoint: legacy_semantic={legacy_semantic_bytes} migrated_semantic={migrated_semantic_bytes} legacy_family={legacy_family_bytes} migrated_family={migrated_family_bytes}; universal_before={before_bytes} universal_after={after_bytes} pending={}=>{}",
            initial_pending,
            fitted.pending_tool_authorities.len()
        );
    }
}
