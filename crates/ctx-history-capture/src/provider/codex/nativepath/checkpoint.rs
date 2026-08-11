use ctx_history_core::TypedKey;
use serde::{Deserialize, Serialize};

use super::rows::{CodexSessionRow, MAX_CODEX_DURABLE_SESSION_ID_BYTES};
use crate::provider::codex::events::CodexInvocationOriginV0;

// The shared family wraps this opaque provider payload in a typed key. Leave
// five bytes for that key's fixed tag and length envelope.
const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 256;
pub(super) const MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
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

const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexCheckpointLineage {
    pub(super) native_session_id: String,
    pub(super) parent_native_session_id: Option<String>,
    pub(super) advisory_session_id: Option<String>,
    pub(super) session_relationship: ctx_history_core::SessionRelationshipKind,
}

/// Provider-only continuation state. Physical position, framing, digests,
/// observations, and lifecycle evidence live exclusively in the enclosing
/// shared JSONL family checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    lineage: CodexCheckpointLineage,
}

impl CodexSemanticCheckpoint {
    pub(super) fn new(owner: &CodexSessionRow) -> Self {
        Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            lineage: CodexCheckpointLineage {
                native_session_id: owner.native_session_id.clone(),
                parent_native_session_id: owner.parent_native_session_id.clone(),
                advisory_session_id: owner.advisory_session_id.clone(),
                session_relationship: owner.session_relationship,
            },
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
        let checkpoint = serde_json::from_slice::<Self>(bytes)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
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

    pub(super) fn owner(&self) -> &CodexCheckpointLineage {
        &self.lineage
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION
            || !checkpoint_lineage_is_valid(&self.lineage)
        {
            return Err(serde::de::Error::custom(
                "invalid Codex semantic checkpoint state",
            ));
        }
        Ok(())
    }
}

fn checkpoint_lineage_is_valid(lineage: &CodexCheckpointLineage) -> bool {
    !lineage.native_session_id.is_empty()
        && lineage.native_session_id.len() <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
        && lineage
            .parent_native_session_id
            .as_ref()
            .is_none_or(|value| {
                !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
            })
        && lineage.advisory_session_id.as_ref().is_none_or(|value| {
            !value.is_empty() && value.len() <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
        })
        && match lineage.session_relationship {
            ctx_history_core::SessionRelationshipKind::Root => {
                lineage.parent_native_session_id.is_none()
            }
            _ => lineage.parent_native_session_id.is_some(),
        }
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

    fn checkpoint() -> CodexSemanticCheckpoint {
        CodexSemanticCheckpoint::new(&owner())
    }

    #[test]
    fn semantic_codec_round_trips_both_key_generations_without_event_bodies() {
        let secret = "event-body-secret-must-not-survive";
        let checkpoint = checkpoint();

        let bytes = checkpoint.encode().unwrap();
        assert_eq!(CodexSemanticCheckpoint::decode(&bytes).unwrap(), checkpoint);
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));
        let wire = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        assert_eq!(wire["version"], CODEX_SEMANTIC_CHECKPOINT_VERSION);
        assert_eq!(wire.as_object().unwrap().len(), 2);
        assert!(wire.get("pending_tool_authorities").is_none());
        assert!(wire.get("local_turn_started").is_none());
        assert!(wire.get("owner").is_none());
        assert_eq!(wire["lineage"].as_object().unwrap().len(), 4);
        assert_eq!(wire["lineage"]["native_session_id"], "checkpoint-owner");

        let compact = checkpoint.encode_key().unwrap();
        assert!(matches!(compact, TypedKey::Utf8(_)));
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&compact).unwrap(),
            checkpoint
        );
        let bytes_key = TypedKey::bytes(bytes).unwrap();
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&bytes_key).unwrap(),
            checkpoint
        );
    }

    #[test]
    fn semantic_codec_rejects_malformed_unknown_version_duplicate_and_oversize() {
        let checkpoint = checkpoint();
        assert!(CodexSemanticCheckpoint::decode(b"{").is_err());
        assert!(CodexSemanticCheckpoint::decode_key(&TypedKey::U64(1)).is_err());

        let mut unknown = serde_json::to_value(&checkpoint).unwrap();
        unknown["unknown"] = json!(true);
        assert!(CodexSemanticCheckpoint::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let mut version = serde_json::to_value(&checkpoint).unwrap();
        version["version"] = json!(CODEX_SEMANTIC_CHECKPOINT_VERSION + 1);
        assert!(CodexSemanticCheckpoint::decode(&serde_json::to_vec(&version).unwrap()).is_err());

        let retired_nested = serde_json::json!({
            "version": 2,
            "pending_tool_authorities": [],
            "owner": owner(),
            "local_turn_started": false
        });
        assert!(
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&retired_nested).unwrap()).is_err()
        );

        assert!(CodexSemanticCheckpoint::decode(&vec![
            b' ';
            MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES + 1
        ])
        .is_err());
        let mut invalid_lineage = serde_json::to_value(&checkpoint).unwrap();
        invalid_lineage["lineage"]["native_session_id"] = json!("");
        assert!(
            CodexSemanticCheckpoint::decode(&serde_json::to_vec(&invalid_lineage).unwrap())
                .is_err()
        );
    }

    #[test]
    fn maximal_semantic_payload_contains_only_bounded_lineage() {
        let owner = maximal_owner();
        let checkpoint = CodexSemanticCheckpoint::new(&owner);
        assert_eq!(
            checkpoint.lineage.native_session_id,
            owner.native_session_id
        );
        assert_eq!(
            checkpoint.lineage.parent_native_session_id,
            owner.parent_native_session_id
        );
        assert_eq!(
            checkpoint.lineage.advisory_session_id,
            owner.advisory_session_id
        );
        assert_eq!(
            checkpoint.lineage.session_relationship,
            owner.session_relationship
        );
        assert!(checkpoint.encode().unwrap().len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES);
    }

    #[test]
    fn maximal_lineage_fits_the_full_family_frontier() {
        let owner = maximal_owner();
        let checkpoint = CodexSemanticCheckpoint::new(&owner);
        let semantic_bytes = checkpoint.encode().unwrap().len();
        let (family_bytes, fits) =
            crate::provider::source_backed::family::jsonl::full_family_checkpoint_frontier_contract_for_test(
                checkpoint.encode_key().unwrap(),
                crate::provider::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES,
            )
            .unwrap();
        assert!(semantic_bytes < 4 * 1024);
        assert!(fits);
        assert!(family_bytes <= 64 * 1024);
        eprintln!("maximal Codex checkpoint: semantic={semantic_bytes} family={family_bytes}");
    }
}
