use std::collections::BTreeSet;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rows::CodexSessionRow;
use crate::provider::codex::events::CodexInvocationOriginV0;

const CODEX_PENDING_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/pending-call-id/v1\0";
const MAX_CODEX_PENDING_TOOL_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;
// The shared family wraps this opaque provider payload in a TypedKey::Bytes.
// Leave five bytes for that key's fixed tag and length envelope.
const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 256;
pub(super) const MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CodexTerminalAuthorityEntry {
    pub(super) call_id_sha256: [u8; 32],
    pub(super) candidates: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexTerminalAuthorityCheckpoint {
    #[serde(with = "terminal_authority_entries_wire")]
    pub(super) mcp_call_ids: Vec<CodexTerminalAuthorityEntry>,
    #[serde(with = "terminal_authority_entries_wire")]
    pub(super) result_call_ids: Vec<CodexTerminalAuthorityEntry>,
    pub(super) mcp_exhausted: bool,
    pub(super) result_exhausted: bool,
}

impl CodexTerminalAuthorityCheckpoint {
    fn validate_wire_state(&self) -> bool {
        fn entries_are_valid(entries: &[CodexTerminalAuthorityEntry]) -> bool {
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
        entries: &[CodexTerminalAuthorityEntry],
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
    ) -> Result<Vec<CodexTerminalAuthorityEntry>, D::Error>
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
                Ok(CodexTerminalAuthorityEntry {
                    call_id_sha256: entry[1..].try_into().map_err(D::Error::custom)?,
                    candidates: entry[0],
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CodexRepositoryCandidateAuthorityEntry {
    pub(super) call_id_sha256: [u8; 32],
    pub(super) calls: u8,
    pub(super) results: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexRepositoryCandidateAuthorityCheckpoint {
    #[serde(with = "repository_candidate_authority_entries_wire")]
    pub(super) entries: Vec<CodexRepositoryCandidateAuthorityEntry>,
    pub(super) exhausted: bool,
}

impl CodexRepositoryCandidateAuthorityCheckpoint {
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
        entries: &[CodexRepositoryCandidateAuthorityEntry],
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
    ) -> Result<Vec<CodexRepositoryCandidateAuthorityEntry>, D::Error>
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
                Ok(CodexRepositoryCandidateAuthorityEntry {
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

const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 1;

/// Provider-only continuation state. Physical position, framing, digests,
/// observations, and lifecycle evidence live exclusively in the enclosing
/// shared JSONL family checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    terminal_authority: CodexTerminalAuthorityCheckpoint,
    repository_candidate_authority: CodexRepositoryCandidateAuthorityCheckpoint,
    owner: CodexSessionRow,
    local_turn_started: bool,
}

impl CodexSemanticCheckpoint {
    pub(super) fn new(
        pending_tool_authorities: &[CodexPendingToolAuthority],
        terminal_authority: CodexTerminalAuthorityCheckpoint,
        repository_candidate_authority: CodexRepositoryCandidateAuthorityCheckpoint,
        owner: CodexSessionRow,
        local_turn_started: bool,
    ) -> serde_json::Result<Self> {
        let mut checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            pending_tool_authorities: pending_tool_authorities.to_vec(),
            terminal_authority,
            repository_candidate_authority,
            owner,
            local_turn_started,
        };
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

    pub(super) fn encode(&self) -> serde_json::Result<Vec<u8>> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        Ok(encoded)
    }

    pub(super) fn decode(bytes: &[u8]) -> serde_json::Result<Self> {
        if bytes.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(bytes.len()));
        }
        let checkpoint = serde_json::from_slice::<Self>(bytes)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
    }

    pub(super) fn pending_tool_authorities(&self) -> &[CodexPendingToolAuthority] {
        &self.pending_tool_authorities
    }

    pub(super) fn terminal_authority(&self) -> &CodexTerminalAuthorityCheckpoint {
        &self.terminal_authority
    }

    pub(super) fn repository_candidate_authority(
        &self,
    ) -> &CodexRepositoryCandidateAuthorityCheckpoint {
        &self.repository_candidate_authority
    }

    pub(super) fn owner(&self) -> &CodexSessionRow {
        &self.owner
    }

    pub(super) fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !self.terminal_authority.validate_wire_state()
            || !self.repository_candidate_authority.validate_wire_state()
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
