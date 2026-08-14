use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ctx_history_core::TypedKey;
use serde::{Deserialize, Serialize};

use super::rows::CodexSessionRow;
use crate::provider::codex::events::{CodexInvocationOriginV0, CodexToolCallContext};

// This is an inner bound only. The shared family applies the real complete
// SourceFrontier bound after adding source identity and physical state. If the
// complete state does not fit either bound, Codex omits it and the next append
// must use exhaustive replay.
pub(super) const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;

pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 256;
pub(super) const MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES: usize = 256;
const MAX_CODEX_REPOSITORY_CANDIDATE_CELLS: usize = MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES;
// At 128 entries the exact digest payload is 4 KiB raw / about 5.5 KiB
// base64. This keeps ordinary checkpoints in the low-KiB range while bounding
// exact insertion and lookup before the fixed Bloom representation takes over.
const MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS: usize = 128;
const CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES: usize = 32 * 1024;
const CODEX_REPOSITORY_OCCURRENCE_BLOOM_HASHES: usize = 11;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexPendingToolAuthority {
    pub(super) raw_ordinal: u64,
    continuation_cell_id: Option<String>,
    continuation_conflicted: bool,
    continuation_call_id_sha256: Vec<[u8; 32]>,
    continuation_capacity_exceeded: bool,
    correlation_ambiguous: bool,
    invocation_origin: CodexInvocationOriginV0,
}

impl CodexPendingToolAuthority {
    pub(super) fn new(raw_ordinal: u64, invocation_origin: CodexInvocationOriginV0) -> Self {
        Self {
            raw_ordinal,
            continuation_cell_id: None,
            continuation_conflicted: false,
            continuation_call_id_sha256: Vec::new(),
            continuation_capacity_exceeded: false,
            correlation_ambiguous: false,
            invocation_origin,
        }
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

    pub(super) fn mark_correlation_ambiguous(&mut self) {
        self.correlation_ambiguous = true;
    }

    pub(super) fn set_invocation_origin(&mut self, origin: CodexInvocationOriginV0) {
        self.invocation_origin = origin;
    }
}

const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 3;

/// Complete provider-owned reducer state at one shared-family physical
/// frontier. Physical position and source/parser bindings remain exclusively
/// in the enclosing `FamilyCheckpoint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    owner: Option<CodexSessionRow>,
    local_turn_started: bool,
    tool_contexts: BTreeMap<String, CodexToolCallContext>,
    tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    continuations: BTreeMap<String, String>,
    mcp_terminal_authority: CodexTerminalAuthorityCheckpoint,
    repository_authority: CodexRepositoryAuthorityCheckpoint,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexTerminalAuthorityCheckpoint {
    pub(super) mcp_call_ids: Vec<CodexDigestMultiplicityCheckpoint>,
    pub(super) result_call_ids: Vec<CodexDigestMultiplicityCheckpoint>,
    pub(super) mcp_exhausted: bool,
    pub(super) result_exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexDigestMultiplicityCheckpoint {
    pub(super) digest: [u8; 32],
    pub(super) count: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexRepositoryMultiplicityCheckpoint {
    pub(super) calls: u8,
    pub(super) results: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexRepositoryAuthorityCheckpoint {
    pub(super) candidates: Vec<CodexRepositoryAuthorityEntryCheckpoint>,
    #[serde(
        default,
        skip_serializing_if = "CodexRepositoryOccurrenceNegativeAuthority::is_empty"
    )]
    pub(super) occurrence_negative_authority: CodexRepositoryOccurrenceNegativeAuthority,
    pub(super) candidate_cells: BTreeSet<String>,
    pub(super) candidate_exhausted: bool,
}

/// Adaptive one-sided authority over every repository call/result digest in
/// the certified prefix. Small sets remain exact and compact. Larger sets
/// promote deterministically to a fixed Bloom filter. A miss is exact absence;
/// a hit must never assert uniqueness and makes a newly admitted suffix
/// candidate retry as a replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CodexRepositoryOccurrenceNegativeAuthority {
    ExactV1 {
        #[serde(with = "repository_occurrence_exact_wire")]
        digests: Vec<[u8; 32]>,
    },
    BloomV1 {
        #[serde(with = "repository_occurrence_bloom_wire")]
        bits: Vec<u8>,
    },
    IncompleteV1,
}

impl Default for CodexRepositoryOccurrenceNegativeAuthority {
    fn default() -> Self {
        Self::ExactV1 {
            digests: Vec::new(),
        }
    }
}

impl CodexRepositoryOccurrenceNegativeAuthority {
    pub(super) fn observe(&mut self, digest: &[u8; 32]) {
        match self {
            Self::ExactV1 { digests } => match digests.binary_search(digest) {
                Ok(_) => {}
                Err(index) if digests.len() < MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS => {
                    digests.insert(index, *digest);
                }
                Err(_) => {
                    let exact = std::mem::take(digests);
                    let mut bits = vec![0; CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES];
                    for observed in exact.iter().chain(std::iter::once(digest)) {
                        observe_repository_occurrence_bloom(&mut bits, observed);
                    }
                    *self = Self::BloomV1 { bits };
                }
            },
            Self::BloomV1 { bits } => observe_repository_occurrence_bloom(bits, digest),
            Self::IncompleteV1 => {}
        }
    }

    pub(super) fn mark_incomplete(&mut self) {
        *self = Self::IncompleteV1;
    }

    pub(super) fn definitely_absent(&self, digest: &[u8; 32]) -> bool {
        match self {
            Self::ExactV1 { digests } => digests.binary_search(digest).is_err(),
            Self::BloomV1 { bits } => repository_occurrence_bloom_bits(digest)
                .into_iter()
                .any(|bit| bits[bit / 8] & (1 << (bit % 8)) == 0),
            Self::IncompleteV1 => false,
        }
    }

    pub(super) fn merge(&mut self, suffix: &Self) {
        match suffix {
            Self::IncompleteV1 => self.mark_incomplete(),
            Self::ExactV1 { digests } => {
                for digest in digests {
                    self.observe(digest);
                }
            }
            Self::BloomV1 { bits: suffix_bits } => {
                if matches!(self, Self::IncompleteV1) {
                    return;
                }
                if let Self::ExactV1 { digests } = self {
                    let exact = std::mem::take(digests);
                    let mut bits = vec![0; CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES];
                    for digest in &exact {
                        observe_repository_occurrence_bloom(&mut bits, digest);
                    }
                    *self = Self::BloomV1 { bits };
                }
                let Self::BloomV1 { bits } = self else {
                    unreachable!("complete occurrence authority promotes to Bloom")
                };
                for (current, suffix) in bits.iter_mut().zip(suffix_bits) {
                    *current |= suffix;
                }
            }
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::ExactV1 { digests } if digests.is_empty())
    }

    fn valid(&self) -> bool {
        match self {
            Self::ExactV1 { digests } => {
                digests.len() <= MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS
                    && digests.windows(2).all(|pair| pair[0] < pair[1])
            }
            Self::BloomV1 { bits } => bits.len() == CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES,
            Self::IncompleteV1 => true,
        }
    }

    #[cfg(test)]
    fn definitely_present(&self, digest: &[u8; 32]) -> bool {
        matches!(self, Self::ExactV1 { digests } if digests.binary_search(digest).is_ok())
    }
}

fn observe_repository_occurrence_bloom(bits: &mut [u8], digest: &[u8; 32]) {
    for bit in repository_occurrence_bloom_bits(digest) {
        bits[bit / 8] |= 1 << (bit % 8);
    }
}

fn repository_occurrence_bloom_bits(
    digest: &[u8; 32],
) -> [usize; CODEX_REPOSITORY_OCCURRENCE_BLOOM_HASHES] {
    let first = u64::from_le_bytes(digest[..8].try_into().expect("fixed SHA-256 slice"));
    let step = u64::from_le_bytes(digest[8..16].try_into().expect("fixed SHA-256 slice")) | 1;
    let mask = u64::try_from(CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES * 8 - 1)
        .expect("Bloom bit count fits u64");
    std::array::from_fn(|index| {
        usize::try_from(first.wrapping_add((index as u64).wrapping_mul(step)) & mask)
            .expect("Bloom bit index fits usize")
    })
}

mod repository_occurrence_bloom_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    pub(super) fn serialize<S>(bits: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(bits))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bits = BASE64_STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if bits.len() != CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES {
            return Err(D::Error::custom(format!(
                "Codex repository occurrence Bloom has {} bytes, expected {}",
                bits.len(),
                CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES,
            )));
        }
        Ok(bits)
    }
}

mod repository_occurrence_exact_wire {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::*;

    pub(super) fn serialize<S>(digests: &[[u8; 32]], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = digests
            .iter()
            .flat_map(|digest| digest.iter().copied())
            .collect::<Vec<_>>();
        serializer.serialize_str(&BASE64_STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = BASE64_STANDARD.decode(encoded).map_err(D::Error::custom)?;
        if bytes.len() % 32 != 0 {
            return Err(D::Error::custom(
                "Codex repository exact occurrence authority is not whole SHA-256 digests",
            ));
        }
        let count = bytes.len() / 32;
        if count > MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS {
            return Err(D::Error::custom(format!(
                "Codex repository exact occurrence authority has {count} digests, maximum is {MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS}",
            )));
        }
        Ok(bytes
            .chunks_exact(32)
            .map(|chunk| chunk.try_into().expect("validated SHA-256 chunk"))
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexRepositoryAuthorityEntryCheckpoint {
    pub(super) digest: [u8; 32],
    pub(super) multiplicity: CodexRepositoryMultiplicityCheckpoint,
}

pub(super) struct CodexSemanticCheckpointState<'a> {
    pub(super) owner: Option<&'a CodexSessionRow>,
    pub(super) local_turn_started: bool,
    pub(super) tool_contexts: &'a BTreeMap<String, CodexToolCallContext>,
    pub(super) tool_authorities: &'a BTreeMap<String, CodexPendingToolAuthority>,
    pub(super) continuations: &'a BTreeMap<String, String>,
    pub(super) mcp_terminal_authority: CodexTerminalAuthorityCheckpoint,
    pub(super) repository_authority: CodexRepositoryAuthorityCheckpoint,
}

impl CodexSemanticCheckpoint {
    pub(super) fn from_state(
        state: CodexSemanticCheckpointState<'_>,
    ) -> serde_json::Result<Option<Self>> {
        let checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            owner: state.owner.cloned(),
            local_turn_started: state.local_turn_started,
            tool_contexts: state.tool_contexts.clone(),
            tool_authorities: state.tool_authorities.clone(),
            continuations: state.continuations.clone(),
            mcp_terminal_authority: state.mcp_terminal_authority,
            repository_authority: state.repository_authority,
        };
        checkpoint.validate_wire_state()?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        Ok((encoded.len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES).then_some(checkpoint))
    }

    #[cfg(test)]
    pub(super) fn encoded_len(&self) -> serde_json::Result<usize> {
        serde_json::to_vec(self).map(|bytes| bytes.len())
    }

    pub(super) fn encode_key(&self) -> serde_json::Result<TypedKey> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        TypedKey::utf8(encoded)
            .map_err(|error| <serde_json::Error as serde::ser::Error>::custom(error.to_string()))
    }

    pub(super) fn decode_key(key: &TypedKey) -> serde_json::Result<Self> {
        let TypedKey::Utf8(encoded) = key else {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "Codex semantic checkpoint is not current UTF-8 state",
            ));
        };
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(checkpoint_size_error(encoded.len()));
        }
        let checkpoint = serde_json::from_str::<Self>(encoded)?;
        checkpoint.validate_wire_state()?;
        // Require one canonical representation. This rejects duplicate fields,
        // noncanonical object ordering, whitespace, and alternate number forms
        // instead of guessing which representation should be authoritative.
        if serde_json::to_string(&checkpoint)? != *encoded {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "Codex semantic checkpoint is not canonical",
            ));
        }
        Ok(checkpoint)
    }

    pub(super) fn direct_append_safe(&self) -> bool {
        self.validate_wire_state().is_ok()
    }

    pub(super) fn owner(&self) -> Option<&CodexSessionRow> {
        self.owner.as_ref()
    }

    pub(super) fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    pub(super) fn tool_contexts(&self) -> &BTreeMap<String, CodexToolCallContext> {
        &self.tool_contexts
    }

    pub(super) fn tool_authorities(&self) -> &BTreeMap<String, CodexPendingToolAuthority> {
        &self.tool_authorities
    }

    pub(super) fn continuations(&self) -> &BTreeMap<String, String> {
        &self.continuations
    }

    pub(super) fn mcp_terminal_authority(&self) -> &CodexTerminalAuthorityCheckpoint {
        &self.mcp_terminal_authority
    }

    pub(super) fn repository_authority(&self) -> &CodexRepositoryAuthorityCheckpoint {
        &self.repository_authority
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        let owner_valid = self.owner.as_ref().is_none_or(valid_owner);
        let tool_keys_valid = self.tool_contexts.len() <= MAX_CODEX_TOOL_CONTEXTS
            && self.tool_contexts.len() == self.tool_authorities.len()
            && self.tool_contexts.keys().eq(self.tool_authorities.keys())
            && self.tool_contexts.keys().all(|call_id| {
                !call_id.is_empty() && call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES
            })
            && self.tool_contexts.values().all(valid_tool_context)
            && self.tool_authorities.values().all(valid_tool_authority);
        let continuations_valid = self.continuations.len() <= MAX_CODEX_TOOL_CONTEXTS
            && self.continuations.iter().all(|(cell_id, origin_call_id)| {
                valid_continuation_cell_id(cell_id)
                    && (origin_call_id.is_empty()
                        || self.tool_contexts.contains_key(origin_call_id))
            });
        let terminal_valid = checkpoint_multiplicities_valid(
            &self.mcp_terminal_authority.mcp_call_ids,
            self.mcp_terminal_authority.mcp_exhausted,
            MAX_CODEX_MCP_TERMINAL_AUTHORITIES,
        ) && checkpoint_multiplicities_valid(
            &self.mcp_terminal_authority.result_call_ids,
            self.mcp_terminal_authority.result_exhausted,
            MAX_CODEX_MCP_TERMINAL_AUTHORITIES,
        );
        let repository_valid = self.repository_authority.candidate_exhausted
            && self.repository_authority.candidates.is_empty()
            && self.repository_authority.candidate_cells.is_empty()
            || !self.repository_authority.candidate_exhausted
                && self.repository_authority.candidates.len()
                    <= MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES
                && repository_multiplicities_valid(self.repository_authority.candidates.iter())
                && self.repository_authority.candidate_cells.len()
                    <= MAX_CODEX_REPOSITORY_CANDIDATE_CELLS
                && self
                    .repository_authority
                    .candidate_cells
                    .iter()
                    .all(|cell_id| valid_continuation_cell_id(cell_id));
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !owner_valid
            || !tool_keys_valid
            || !continuations_valid
            || !terminal_valid
            || !repository_valid
            || !self
                .repository_authority
                .occurrence_negative_authority
                .valid()
        {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "invalid Codex semantic checkpoint state",
            ));
        }
        Ok(())
    }
}

fn valid_owner(owner: &CodexSessionRow) -> bool {
    use super::rows::{
        MAX_CODEX_DURABLE_CWD_BYTES, MAX_CODEX_DURABLE_METADATA_BYTES,
        MAX_CODEX_DURABLE_SESSION_ID_BYTES,
    };

    !owner.native_session_id.is_empty()
        && owner.native_session_id.len() <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
        && [
            owner.parent_native_session_id.as_deref(),
            owner.root_native_session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_SESSION_ID_BYTES)
        && owner
            .cwd
            .as_deref()
            .is_none_or(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_CWD_BYTES)
        && [
            owner.originator.as_deref(),
            owner.cli_version.as_deref(),
            owner.source_kind.as_deref(),
            owner.external_agent_id.as_deref(),
            owner.role_hint.as_deref(),
            owner.model_provider.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)
        && owner.git.as_ref().is_none_or(|git| {
            [
                git.commit_hash.as_deref(),
                git.branch.as_deref(),
                git.repository_url.as_deref(),
            ]
            .into_iter()
            .flatten()
            .all(|value| !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_METADATA_BYTES)
        })
}

fn valid_tool_context(context: &CodexToolCallContext) -> bool {
    let continuation_digests_valid = context.continuation_call_id_sha256.len()
        <= MAX_CODEX_TOOL_CONTEXTS
        && context
            .continuation_call_id_sha256
            .iter()
            .all(|digest| *digest != [0; 32])
        && context
            .continuation_call_id_sha256
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            == context.continuation_call_id_sha256.len();
    context.tool_name.len() <= super::reader::MAX_CODEX_TOOL_NAME_BYTES
        && [
            context.command_preview.as_deref(),
            context.arguments_preview.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|value| value.len() <= super::reader::MAX_CODEX_TOOL_PREVIEW_BYTES)
        && context
            .exact_command
            .as_deref()
            .is_none_or(|value| value.len() <= crate::repository_attribution::MAX_COMMAND_BYTES)
        && [
            context.session_cwd.as_deref(),
            context.declared_workdir.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|value| value.len() <= 16 * 1024)
        && context
            .continuation_cell_id
            .as_deref()
            .is_none_or(valid_continuation_cell_id)
        && context.origin_call_id.as_deref().is_none_or(|call_id| {
            !call_id.is_empty() && call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES
        })
        && match &context.invocation_origin {
            CodexInvocationOriginV0::CopiedFromAncestor {
                ancestor_native_session_id,
            } => {
                !ancestor_native_session_id.is_empty()
                    && ancestor_native_session_id.len()
                        <= super::rows::MAX_CODEX_DURABLE_SESSION_ID_BYTES
            }
            CodexInvocationOriginV0::UniqueToSession | CodexInvocationOriginV0::Unproven => true,
        }
        && continuation_digests_valid
}

fn valid_tool_authority(authority: &CodexPendingToolAuthority) -> bool {
    authority
        .continuation_cell_id
        .as_deref()
        .is_none_or(valid_continuation_cell_id)
        && authority.continuation_call_id_sha256.len() <= MAX_CODEX_TOOL_CONTEXTS
        && authority
            .continuation_call_id_sha256
            .iter()
            .all(|digest| *digest != [0; 32])
        && authority
            .continuation_call_id_sha256
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            == authority.continuation_call_id_sha256.len()
        && match &authority.invocation_origin {
            CodexInvocationOriginV0::CopiedFromAncestor {
                ancestor_native_session_id,
            } => {
                !ancestor_native_session_id.is_empty()
                    && ancestor_native_session_id.len()
                        <= super::rows::MAX_CODEX_DURABLE_SESSION_ID_BYTES
            }
            CodexInvocationOriginV0::UniqueToSession | CodexInvocationOriginV0::Unproven => true,
        }
}

fn checkpoint_multiplicities_valid(
    entries: &[CodexDigestMultiplicityCheckpoint],
    exhausted: bool,
    maximum: usize,
) -> bool {
    entries.len() <= maximum
        && entries.iter().all(|entry| matches!(entry.count, 1 | 2))
        && entries
            .windows(2)
            .all(|entries| entries[0].digest < entries[1].digest)
        && (!exhausted || entries.is_empty())
}

fn repository_multiplicities_valid<'a>(
    entries: impl Iterator<Item = &'a CodexRepositoryAuthorityEntryCheckpoint>,
) -> bool {
    let entries = entries.collect::<Vec<_>>();
    entries.iter().all(|entry| {
        entry.multiplicity.calls <= 2
            && entry.multiplicity.results <= 2
            && entry.multiplicity.calls + entry.multiplicity.results != 0
    }) && entries
        .windows(2)
        .all(|entries| entries[0].digest < entries[1].digest)
}

fn valid_continuation_cell_id(cell_id: &str) -> bool {
    !cell_id.is_empty()
        && cell_id.len() <= MAX_CODEX_CONTINUATION_CELL_ID_BYTES
        && cell_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn checkpoint_size_error(actual: usize) -> serde_json::Error {
    <serde_json::Error as serde::ser::Error>::custom(format!(
        "Codex semantic checkpoint payload has {actual} bytes, maximum is {MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES}"
    ))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use ctx_history_core::SessionRelationshipKind;
    use sha2::Digest as _;

    use super::*;
    use crate::provider::codex::nativepath::rows::CodexSessionGitMetadata;

    fn owner() -> CodexSessionRow {
        CodexSessionRow {
            native_session_id: "checkpoint-session".to_owned(),
            parent_native_session_id: Some("checkpoint-parent".to_owned()),
            root_native_session_id: Some("checkpoint-root".to_owned()),
            session_relationship: SessionRelationshipKind::Forked,
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            cwd: Some("/tmp/checkpoint".to_owned()),
            originator: Some("codex_cli_rs".to_owned()),
            cli_version: Some("1.0.0".to_owned()),
            source_kind: Some("cli".to_owned()),
            external_agent_id: Some("agent".to_owned()),
            role_hint: Some("worker".to_owned()),
            model_provider: Some("openai".to_owned()),
            git: Some(CodexSessionGitMetadata {
                commit_hash: Some("0123456789abcdef".to_owned()),
                branch: Some("main".to_owned()),
                repository_url: Some("https://example.invalid/repository".to_owned()),
            }),
        }
    }

    fn checkpoint_with_command(command: &str) -> Option<CodexSemanticCheckpoint> {
        checkpoint_with_command_and_occurrences(
            command,
            CodexRepositoryOccurrenceNegativeAuthority::default(),
        )
    }

    fn checkpoint_with_command_and_occurrences(
        command: &str,
        occurrence_negative_authority: CodexRepositoryOccurrenceNegativeAuthority,
    ) -> Option<CodexSemanticCheckpoint> {
        let owner = owner();
        let continuation_digest = [9; 32];
        let context = CodexToolCallContext {
            tool_name: "exec_command".to_owned(),
            command_preview: Some("git status".to_owned()),
            arguments_preview: Some("{\"cmd\":\"git status\"}".to_owned()),
            exact_command: Some(command.to_owned()),
            command_too_large: false,
            session_cwd: Some("/tmp/checkpoint".to_owned()),
            declared_workdir: Some("repository".to_owned()),
            continuation_cell_id: Some("cell-1".to_owned()),
            origin_call_id: Some("call-1".to_owned()),
            origin_event_sequence: Some(7),
            origin_occurred_at_unix_ms: Some(1_700_000_000_000),
            continuation_call_id_sha256: vec![continuation_digest],
            continuation_capacity_exceeded: false,
            correlation_ambiguous: false,
            invocation_origin: CodexInvocationOriginV0::CopiedFromAncestor {
                ancestor_native_session_id: "checkpoint-parent".to_owned(),
            },
        };
        let mut authority = CodexPendingToolAuthority::new(
            4,
            CodexInvocationOriginV0::CopiedFromAncestor {
                ancestor_native_session_id: "checkpoint-parent".to_owned(),
            },
        );
        assert!(authority.assign_continuation("cell-1"));
        authority.record_continuation_call(continuation_digest);
        let tool_contexts = BTreeMap::from([("call-1".to_owned(), context)]);
        let tool_authorities = BTreeMap::from([("call-1".to_owned(), authority)]);
        let continuations = BTreeMap::from([("cell-1".to_owned(), "call-1".to_owned())]);
        CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: true,
            tool_contexts: &tool_contexts,
            tool_authorities: &tool_authorities,
            continuations: &continuations,
            mcp_terminal_authority: CodexTerminalAuthorityCheckpoint {
                mcp_call_ids: vec![CodexDigestMultiplicityCheckpoint {
                    digest: [1; 32],
                    count: 1,
                }],
                result_call_ids: vec![CodexDigestMultiplicityCheckpoint {
                    digest: [2; 32],
                    count: 2,
                }],
                mcp_exhausted: false,
                result_exhausted: false,
            },
            repository_authority: CodexRepositoryAuthorityCheckpoint {
                candidates: vec![CodexRepositoryAuthorityEntryCheckpoint {
                    digest: [3; 32],
                    multiplicity: CodexRepositoryMultiplicityCheckpoint {
                        calls: 1,
                        results: 1,
                    },
                }],
                occurrence_negative_authority,
                candidate_cells: BTreeSet::from(["cell-1".to_owned()]),
                candidate_exhausted: false,
            },
        })
        .unwrap()
    }

    fn occurrence_authority(distinct: usize) -> CodexRepositoryOccurrenceNegativeAuthority {
        let mut authority = CodexRepositoryOccurrenceNegativeAuthority::default();
        for index in 0..distinct {
            authority.observe(&sha2::Sha256::digest(index.to_le_bytes()).into());
        }
        authority
    }

    fn rejected_wire(mut edit: impl FnMut(&mut serde_json::Value)) {
        let checkpoint = checkpoint_with_command("git status").unwrap();
        let mut wire = serde_json::to_value(checkpoint).unwrap();
        edit(&mut wire);
        let key = TypedKey::Utf8(serde_json::to_string(&wire).unwrap());
        assert!(CodexSemanticCheckpoint::decode_key(&key).is_err());
    }

    #[test]
    fn current_checkpoint_roundtrips_canonically_with_complete_reducer_state() {
        let checkpoint = checkpoint_with_command("git status").unwrap();
        let typical_bytes = checkpoint.encoded_len().unwrap();
        assert!(
            typical_bytes < 3 * 1024,
            "typical checkpoint was {typical_bytes} bytes"
        );
        let key = checkpoint.encode_key().unwrap();
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&key).unwrap(),
            checkpoint
        );
        let TypedKey::Utf8(encoded) = key else {
            panic!("current Codex semantic checkpoint must use UTF-8");
        };
        assert_eq!(encoded.len(), typical_bytes);
        eprintln!(
            "Codex semantic checkpoint: typical={typical_bytes} max={MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES}"
        );
    }

    #[test]
    fn checkpoint_is_complete_or_omitted_at_the_bound() {
        assert!(checkpoint_with_command("git status").is_some());
        assert!(checkpoint_with_command(&"x".repeat(128 * 1024)).is_none());
        let oversized = TypedKey::Utf8("x".repeat(MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES + 1));
        assert!(CodexSemanticCheckpoint::decode_key(&oversized).is_err());
    }

    #[test]
    fn legacy_malformed_noncanonical_and_wrong_typed_keys_are_rejected() {
        let legacy = TypedKey::Utf8("{\"version\":0}".to_owned());
        assert!(CodexSemanticCheckpoint::decode_key(&legacy).is_err());
        assert!(CodexSemanticCheckpoint::decode_key(&TypedKey::U64(1)).is_err());

        let checkpoint = checkpoint_with_command("git status").unwrap();
        let TypedKey::Utf8(canonical) = checkpoint.encode_key().unwrap() else {
            unreachable!();
        };
        assert!(
            CodexSemanticCheckpoint::decode_key(&TypedKey::Utf8(format!("{canonical}\n"))).is_err()
        );
        let duplicate_version =
            canonical.replacen("{\"version\":3,", "{\"version\":3,\"version\":3,", 1);
        assert!(CodexSemanticCheckpoint::decode_key(&TypedKey::Utf8(duplicate_version)).is_err());
    }

    #[test]
    fn invalid_authority_and_repository_states_are_rejected_fail_closed() {
        rejected_wire(|wire| wire["version"] = serde_json::json!(4));
        rejected_wire(|wire| {
            wire["mcp_terminal_authority"]["mcp_call_ids"][0]["count"] = serde_json::json!(0)
        });
        rejected_wire(|wire| {
            wire["repository_authority"]["candidate_exhausted"] = serde_json::json!(true)
        });
        rejected_wire(|wire| {
            wire["tool_authorities"] = serde_json::json!({});
        });
        rejected_wire(|wire| {
            wire["tool_contexts"]["call-1"]["continuation_call_id_sha256"] =
                serde_json::to_value(vec![[0_u8; 32]]).unwrap()
        });
        rejected_wire(|wire| {
            wire["repository_authority"]["occurrence_negative_authority"] = serde_json::json!({
                "complete": true,
                "bits": BASE64_STANDARD.encode(vec![0; CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES]),
            })
        });
        rejected_wire(|wire| {
            let duplicate = [[7_u8; 32], [7_u8; 32]]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            wire["repository_authority"]["occurrence_negative_authority"] = serde_json::json!({
                "kind": "exact_v1",
                "digests": BASE64_STANDARD.encode(duplicate),
            })
        });
        rejected_wire(|wire| {
            wire["repository_authority"]["occurrence_negative_authority"] = serde_json::json!({
                "kind": "bloom_v1",
                "bits": BASE64_STANDARD.encode([0_u8; 31]),
            })
        });
    }

    #[test]
    fn explicit_exhausted_abstention_states_roundtrip() {
        let owner = owner();
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: false,
            tool_contexts: &BTreeMap::new(),
            tool_authorities: &BTreeMap::new(),
            continuations: &BTreeMap::new(),
            mcp_terminal_authority: CodexTerminalAuthorityCheckpoint {
                mcp_call_ids: Vec::new(),
                result_call_ids: Vec::new(),
                mcp_exhausted: true,
                result_exhausted: true,
            },
            repository_authority: CodexRepositoryAuthorityCheckpoint {
                candidates: Vec::new(),
                occurrence_negative_authority: CodexRepositoryOccurrenceNegativeAuthority::default(
                ),
                candidate_cells: BTreeSet::new(),
                candidate_exhausted: true,
            },
        })
        .unwrap()
        .unwrap();
        let restored =
            CodexSemanticCheckpoint::decode_key(&checkpoint.encode_key().unwrap()).unwrap();
        assert!(restored.mcp_terminal_authority.mcp_exhausted);
        assert!(restored.mcp_terminal_authority.result_exhausted);
        assert!(restored.repository_authority.candidate_exhausted);
    }

    #[test]
    fn occurrence_negative_authority_is_adaptive_exact_then_one_sided_bloom() {
        let mut authority = CodexRepositoryOccurrenceNegativeAuthority::default();
        let observed = [7; 32];
        authority.observe(&observed);
        let absent = [8; 32];
        assert!(authority.definitely_present(&observed));
        assert!(!authority.definitely_absent(&observed));
        assert!(authority.definitely_absent(&absent));

        authority = occurrence_authority(MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS);
        assert!(matches!(
            authority,
            CodexRepositoryOccurrenceNegativeAuthority::ExactV1 { .. }
        ));
        let promoted =
            sha2::Sha256::digest(MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS.to_le_bytes())
                .into();
        authority.observe(&promoted);
        assert!(matches!(
            authority,
            CodexRepositoryOccurrenceNegativeAuthority::BloomV1 { .. }
        ));
        for index in 0..=MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS {
            let digest = sha2::Sha256::digest(index.to_le_bytes()).into();
            assert!(
                !authority.definitely_absent(&digest),
                "promotion lost digest {index}"
            );
        }

        let mut merged = occurrence_authority(100);
        let mut suffix = CodexRepositoryOccurrenceNegativeAuthority::default();
        for index in 100_usize..160 {
            suffix.observe(&sha2::Sha256::digest(index.to_le_bytes()).into());
        }
        merged.merge(&suffix);
        assert!(matches!(
            merged,
            CodexRepositoryOccurrenceNegativeAuthority::BloomV1 { .. }
        ));
        for index in 0_usize..160 {
            let digest = sha2::Sha256::digest(index.to_le_bytes()).into();
            assert!(
                !merged.definitely_absent(&digest),
                "merge lost digest {index}"
            );
        }

        authority.mark_incomplete();
        assert!(!authority.definitely_absent(&absent));
        assert_eq!(
            serde_json::to_string(&authority).unwrap(),
            "{\"kind\":\"incomplete_v1\"}"
        );
    }

    #[test]
    fn occurrence_authority_checkpoint_sizes_are_adaptive_and_bounded() {
        const ORDINARY_OCCURRENCES: usize = 16;
        let counts = [
            0,
            1,
            ORDINARY_OCCURRENCES,
            MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS,
            16_384,
            20_000,
        ];
        let sizes = counts.map(|count| {
            checkpoint_with_command_and_occurrences("git status", occurrence_authority(count))
                .unwrap_or_else(|| panic!("checkpoint omitted at {count} occurrences"))
                .encoded_len()
                .unwrap()
        });

        assert!(
            sizes[0] < 3 * 1024,
            "empty checkpoint was {} bytes",
            sizes[0]
        );
        assert!(
            sizes[1] < 3 * 1024,
            "one-occurrence checkpoint was {} bytes",
            sizes[1]
        );
        assert!(
            sizes[2] < 4 * 1024,
            "ordinary checkpoint was {} bytes",
            sizes[2]
        );
        assert!(
            sizes[3] < 8 * 1024,
            "threshold checkpoint was {} bytes",
            sizes[3]
        );
        assert!(sizes[4] < MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES);
        assert_eq!(sizes[4], sizes[5], "Bloom wire size must remain fixed");

        let empty = checkpoint_with_command_and_occurrences(
            "git status",
            CodexRepositoryOccurrenceNegativeAuthority::default(),
        )
        .unwrap();
        let wire = serde_json::to_value(empty).unwrap();
        assert!(
            wire["repository_authority"]
                .get("occurrence_negative_authority")
                .is_none(),
            "empty occurrence authority must be omitted"
        );
        eprintln!(
            "Codex adaptive occurrence checkpoints: 0={} 1={} ordinary({ORDINARY_OCCURRENCES})={} threshold({MAX_CODEX_REPOSITORY_OCCURRENCE_EXACT_DIGESTS})={} 16384={} 20000={} bytes",
            sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5],
        );
    }

    #[test]
    fn thousand_ordinary_sources_do_not_publish_a_bloom_per_source() {
        const SOURCES: usize = 1_000;
        let empty_bytes = checkpoint_with_command("git status")
            .unwrap()
            .encoded_len()
            .unwrap()
            * SOURCES;
        let mut ordinary_bytes = 0;
        let mut occurrence_count = 0;
        for source in 0..SOURCES {
            let occurrences = source % 17;
            occurrence_count += occurrences;
            ordinary_bytes += checkpoint_with_command_and_occurrences(
                "git status",
                occurrence_authority(occurrences),
            )
            .unwrap()
            .encoded_len()
            .unwrap();
        }
        let fixed_raw_bloom_floor = SOURCES * CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES;
        let fixed_checkpoint_bytes = checkpoint_with_command_and_occurrences(
            "git status",
            CodexRepositoryOccurrenceNegativeAuthority::BloomV1 {
                bits: vec![0; CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES],
            },
        )
        .unwrap()
        .encoded_len()
        .unwrap()
            * SOURCES;
        assert!(ordinary_bytes > empty_bytes);
        assert!(ordinary_bytes < 4 * 1024 * SOURCES);
        assert!(ordinary_bytes < fixed_raw_bloom_floor / 8);
        assert!(ordinary_bytes < fixed_checkpoint_bytes / 16);
        eprintln!(
            "Codex 1k-source checkpoint proxy: occurrences={occurrence_count} empty_total={empty_bytes} ordinary_total={ordinary_bytes} fixed_raw_bloom_floor={fixed_raw_bloom_floor} fixed_checkpoint_total={fixed_checkpoint_bytes} bytes"
        );
    }

    #[test]
    fn occurrence_negative_authority_has_useful_modeled_false_positive_ceiling() {
        fn modeled_false_positive_rate(distinct: usize) -> f64 {
            let bits = (CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES * 8) as f64;
            let hashes = CODEX_REPOSITORY_OCCURRENCE_BLOOM_HASHES as f64;
            (1.0 - (-hashes * distinct as f64 / bits).exp()).powf(hashes)
        }

        let at_16k = modeled_false_positive_rate(16_384);
        let at_20k = modeled_false_positive_rate(20_000);
        assert!(at_16k < 0.0005, "16k modeled FP rate was {at_16k:.6}");
        assert!(at_20k < 0.0021, "20k modeled FP rate was {at_20k:.6}");
        eprintln!(
            "Codex occurrence Bloom: bytes={} hashes={} fp@16384={at_16k:.6} fp@20000={at_20k:.6}",
            CODEX_REPOSITORY_OCCURRENCE_BLOOM_BYTES, CODEX_REPOSITORY_OCCURRENCE_BLOOM_HASHES,
        );
    }
}
