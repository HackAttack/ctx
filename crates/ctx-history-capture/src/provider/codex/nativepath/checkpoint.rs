use std::collections::BTreeSet;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rows::CodexSessionRow;
use super::source::CodexFileObservation;

const CODEX_NATIVE_CHECKPOINT_VERSION: u8 = 14;
const CODEX_PENDING_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/pending-call-id/v1\0";
const MAX_CODEX_PENDING_TOOL_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;
// SourceFrontier encodes a TypedKey::Bytes as one tag byte, one four-byte
// length, and this payload. Keeping the payload five bytes below Core's fixed
// 64 KiB source-checkpoint ceiling makes the entire encoded frontier key fit.
pub(crate) const MAX_CODEX_NATIVE_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 256;

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
    pub(super) fn new(call_id: &str, record_start: u64, record_end: u64, raw_ordinal: u64) -> Self {
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
        }
    }

    pub(super) fn matches_call_id(&self, call_id: &str) -> bool {
        Self::new(
            call_id,
            self.record_start,
            self.record_end,
            self.raw_ordinal,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CodexCheckpointBoundary {
    Terminal {
        complete_eof: u64,
    },
    Incomplete {
        complete_prefix_end: u64,
        incomplete_tail_len: u64,
        incomplete_tail_sha256: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexNativeCheckpoint {
    version: u8,
    pub(crate) observation: CodexFileObservation,
    pub(crate) full_revision_sha256: [u8; 32],
    pub(crate) complete_prefix_sha256: [u8; 32],
    boundary: CodexCheckpointBoundary,
    complete_record_count: u64,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    terminal_authority: CodexTerminalAuthorityCheckpoint,
    pub(crate) owner: CodexSessionRow,
    local_turn_started: bool,
}

impl CodexNativeCheckpoint {
    #[allow(
        clippy::too_many_arguments,
        reason = "the checkpoint constructor mirrors its fixed, versioned wire fields"
    )]
    pub(super) fn new(
        observation: CodexFileObservation,
        full_revision_sha256: [u8; 32],
        complete_prefix_sha256: [u8; 32],
        complete_prefix_end: u64,
        complete_record_count: u64,
        incomplete_tail: Option<(u64, [u8; 32])>,
        pending_tool_authorities: &[CodexPendingToolAuthority],
        terminal_authority: CodexTerminalAuthorityCheckpoint,
        owner: CodexSessionRow,
        local_turn_started: bool,
    ) -> serde_json::Result<Self> {
        let boundary = match incomplete_tail {
            Some((incomplete_tail_len, incomplete_tail_sha256)) => {
                CodexCheckpointBoundary::Incomplete {
                    complete_prefix_end,
                    incomplete_tail_len,
                    incomplete_tail_sha256,
                }
            }
            None => CodexCheckpointBoundary::Terminal {
                complete_eof: complete_prefix_end,
            },
        };
        let mut checkpoint = Self {
            version: CODEX_NATIVE_CHECKPOINT_VERSION,
            observation,
            full_revision_sha256,
            complete_prefix_sha256,
            boundary,
            complete_record_count,
            pending_tool_authorities: pending_tool_authorities.to_vec(),
            terminal_authority,
            owner,
            local_turn_started,
        };
        loop {
            let encoded = serde_json::to_vec(&checkpoint)?;
            if encoded.len() <= MAX_CODEX_NATIVE_CHECKPOINT_BYTES {
                return Ok(checkpoint);
            }
            // Pending invocations are optional future-correlation evidence.
            // Shedding one can only make a later result unjoined/Unknown; it
            // cannot create a positive attribution. Terminal multiplicities
            // and owner identity are never shed.
            if checkpoint.pending_tool_authorities.pop().is_none() {
                return Err(checkpoint_size_error(encoded.len()));
            }
        }
    }

    pub(crate) fn encode(&self) -> serde_json::Result<Vec<u8>> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_CODEX_NATIVE_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        Ok(encoded)
    }

    pub(crate) fn decode(bytes: &[u8]) -> serde_json::Result<Self> {
        if bytes.len() > MAX_CODEX_NATIVE_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(bytes.len()));
        }
        let checkpoint = serde_json::from_slice::<Self>(bytes)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
    }

    pub(crate) fn complete_prefix_end(&self) -> u64 {
        match self.boundary {
            CodexCheckpointBoundary::Terminal { complete_eof } => complete_eof,
            CodexCheckpointBoundary::Incomplete {
                complete_prefix_end,
                ..
            } => complete_prefix_end,
        }
    }

    pub(crate) fn next_raw_ordinal(&self) -> u64 {
        self.complete_record_count
    }

    pub(crate) fn incomplete_tail(&self) -> Option<(u64, [u8; 32])> {
        match self.boundary {
            CodexCheckpointBoundary::Terminal { .. } => None,
            CodexCheckpointBoundary::Incomplete {
                incomplete_tail_len,
                incomplete_tail_sha256,
                ..
            } => Some((incomplete_tail_len, incomplete_tail_sha256)),
        }
    }

    pub(super) fn pending_tool_authorities(&self) -> &[CodexPendingToolAuthority] {
        &self.pending_tool_authorities
    }

    pub(super) fn terminal_authority(&self) -> &CodexTerminalAuthorityCheckpoint {
        &self.terminal_authority
    }

    pub(crate) fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        if self.version != CODEX_NATIVE_CHECKPOINT_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported Codex NativePath checkpoint version {}",
                self.version
            )));
        }
        match self.boundary {
            CodexCheckpointBoundary::Terminal { complete_eof }
                if complete_eof == self.observation.len => {}
            CodexCheckpointBoundary::Incomplete {
                complete_prefix_end,
                incomplete_tail_len,
                ..
            } if incomplete_tail_len != 0
                && complete_prefix_end
                    .checked_add(incomplete_tail_len)
                    .is_some_and(|end| end == self.observation.len) => {}
            _ => {
                return Err(serde::de::Error::custom(
                    "invalid Codex NativePath checkpoint boundary state",
                ));
            }
        }
        let mut call_ids = BTreeSet::new();
        let mut record_spans = BTreeSet::new();
        let mut raw_ordinals = BTreeSet::new();
        let mut continuation_cells = BTreeSet::new();
        if !self.terminal_authority.validate_wire_state() {
            return Err(serde::de::Error::custom(
                "Codex NativePath checkpoint terminal authority is invalid",
            ));
        }
        if self.pending_tool_authorities.len() > MAX_CODEX_TOOL_CONTEXTS
            || self.pending_tool_authorities.iter().any(|authority| {
                authority.record_start >= authority.record_end
                    || authority.record_end > self.complete_prefix_end()
                    || authority.record_end.saturating_sub(authority.record_start)
                        > MAX_CODEX_PENDING_TOOL_RECORD_BYTES
                    || authority.raw_ordinal >= self.complete_record_count
                    || authority.call_id_sha256 == [0; 32]
                    || !call_ids.insert(authority.call_id_sha256)
                    || !record_spans.insert((authority.record_start, authority.record_end))
                    || !raw_ordinals.insert(authority.raw_ordinal)
                    || authority
                        .continuation_cell_id
                        .as_ref()
                        .is_some_and(|cell_id| {
                            cell_id.is_empty()
                                || cell_id.len() > MAX_CODEX_CONTINUATION_CELL_ID_BYTES
                                || !cell_id.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric()
                                        || matches!(byte, b'-' | b'_' | b'.' | b':')
                                })
                                || !continuation_cells.insert(cell_id.clone())
                        })
                    || authority.continuation_call_id_sha256.len() > MAX_CODEX_TOOL_CONTEXTS
                    || (authority.continuation_capacity_exceeded
                        && authority.continuation_call_id_sha256.len() != MAX_CODEX_TOOL_CONTEXTS)
                    || (authority.continuation_conflicted
                        && authority.continuation_cell_id.is_none())
                    || authority.continuation_call_id_sha256.contains(&[0; 32])
                    || authority
                        .continuation_call_id_sha256
                        .iter()
                        .collect::<BTreeSet<_>>()
                        .len()
                        != authority.continuation_call_id_sha256.len()
            })
        {
            return Err(serde::de::Error::custom(
                "Codex NativePath checkpoint pending-tool authority is invalid",
            ));
        }
        Ok(())
    }
}

fn checkpoint_size_error(actual: usize) -> serde_json::Error {
    <serde_json::Error as serde::ser::Error>::custom(format!(
        "Codex NativePath checkpoint payload has {actual} bytes, maximum is {MAX_CODEX_NATIVE_CHECKPOINT_BYTES}"
    ))
}
