use std::collections::BTreeMap;

use ctx_history_core::TypedKey;
use serde::{Deserialize, Serialize};

use super::rows::{CodexSessionRow, MAX_CODEX_DURABLE_SESSION_ID_BYTES};

pub(super) const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 5;
const CODEX_SEMANTIC_CHECKPOINT_PREFIX: &str = "codex.projector-checkpoint.v5:";
pub(super) const MAX_CODEX_PENDING_CALLS: usize = 24;
pub(super) const MAX_CODEX_CALL_ID_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CodexPendingCallOriginV0 {
    CurrentSession,
    CopiedFromAncestor { ancestor_native_session_id: String },
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexPendingCallV0 {
    pub(super) raw_ordinal: u64,
    pub(super) origin: CodexPendingCallOriginV0,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    owner: Option<CodexSessionRow>,
    local_turn_started: bool,
    pending_calls: BTreeMap<String, CodexPendingCallV0>,
}

pub(super) struct CodexSemanticCheckpointState<'a> {
    pub(super) owner: Option<&'a CodexSessionRow>,
    pub(super) local_turn_started: bool,
    pub(super) pending_calls: &'a BTreeMap<String, CodexPendingCallV0>,
}

impl CodexSemanticCheckpoint {
    pub(super) fn from_state(
        state: CodexSemanticCheckpointState<'_>,
    ) -> Result<Option<Self>, serde_json::Error> {
        let checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            owner: state.owner.cloned(),
            local_turn_started: state.local_turn_started,
            pending_calls: state.pending_calls.clone(),
        };
        checkpoint.validate_wire_state()?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        Ok((encoded.len() <= MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES).then_some(checkpoint))
    }

    pub(super) fn encode_key(&self) -> Result<TypedKey, serde_json::Error> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(size_error(encoded.len()));
        }
        TypedKey::utf8(format!("{CODEX_SEMANTIC_CHECKPOINT_PREFIX}{encoded}"))
            .map_err(|error| serde_json::Error::io(std::io::Error::other(error.to_string())))
    }

    pub(super) fn decode_key(key: &TypedKey) -> Result<Self, serde_json::Error> {
        let TypedKey::Utf8(value) = key else {
            return Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Codex semantic checkpoint must be UTF-8",
            )));
        };
        let encoded = value
            .strip_prefix(CODEX_SEMANTIC_CHECKPOINT_PREFIX)
            .ok_or_else(|| {
                serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Codex semantic checkpoint prefix is invalid",
                ))
            })?;
        if encoded.len() > MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES {
            return Err(size_error(encoded.len()));
        }
        let checkpoint: Self = serde_json::from_str(encoded)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
    }

    pub(super) fn direct_append_safe(&self) -> bool {
        self.owner.is_some() && self.validate_wire_state().is_ok()
    }

    pub(super) fn owner(&self) -> Option<&CodexSessionRow> {
        self.owner.as_ref()
    }

    pub(super) const fn local_turn_started(&self) -> bool {
        self.local_turn_started
    }

    pub(super) fn pending_calls(&self) -> &BTreeMap<String, CodexPendingCallV0> {
        &self.pending_calls
    }

    fn validate_wire_state(&self) -> Result<(), serde_json::Error> {
        let pending_calls_valid = self.pending_calls.len() <= MAX_CODEX_PENDING_CALLS
            && self.pending_calls.iter().all(|(call_id, pending)| {
                !call_id.is_empty()
                    && call_id.len() <= MAX_CODEX_CALL_ID_BYTES
                    && match &pending.origin {
                        CodexPendingCallOriginV0::CurrentSession
                        | CodexPendingCallOriginV0::Unproven => true,
                        CodexPendingCallOriginV0::CopiedFromAncestor {
                            ancestor_native_session_id,
                        } => {
                            !ancestor_native_session_id.is_empty()
                                && ancestor_native_session_id.len()
                                    <= MAX_CODEX_DURABLE_SESSION_ID_BYTES
                                && self.owner.as_ref().is_some_and(|owner| {
                                    owner.native_session_id != *ancestor_native_session_id
                                        && owner.parent_native_session_id.as_deref()
                                            == Some(ancestor_native_session_id.as_str())
                                })
                        }
                    }
            });
        if self.version != CODEX_SEMANTIC_CHECKPOINT_VERSION || !pending_calls_valid {
            return Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Codex semantic checkpoint state is invalid",
            )));
        }
        Ok(())
    }
}

fn size_error(actual: usize) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Codex semantic checkpoint is {actual} bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ctx_history_core::ProviderNativeSessionRelationship;

    fn owner() -> CodexSessionRow {
        CodexSessionRow {
            native_session_id: "019fb100-0000-7000-8000-000000000002".to_owned(),
            parent_native_session_id: Some("019fb100-0000-7000-8000-000000000001".to_owned()),
            root_native_session_id: None,
            session_relationship: Some(ProviderNativeSessionRelationship::Forked),
            started_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            cwd: None,
            originator: None,
            cli_version: None,
            source_kind: None,
            external_agent_id: None,
            role_hint: None,
            model_provider: None,
            git: None,
        }
    }

    #[test]
    fn neutral_checkpoint_round_trip_contains_only_scanner_continuation() {
        let pending_calls = BTreeMap::new();
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: None,
            local_turn_started: false,
            pending_calls: &pending_calls,
        })
        .unwrap()
        .unwrap();
        let key = checkpoint.encode_key().unwrap();
        let TypedKey::Utf8(encoded) = &key else {
            panic!("Codex checkpoint must stay UTF-8");
        };

        assert!(encoded.starts_with(CODEX_SEMANTIC_CHECKPOINT_PREFIX));
        assert!(!encoded.contains("repository"));
        assert!(!encoded.contains("confidence"));
        assert!(!encoded.contains("effect"));
        assert_eq!(
            CodexSemanticCheckpoint::decode_key(&key).unwrap(),
            checkpoint
        );
        assert!(!checkpoint.direct_append_safe());
    }

    #[test]
    fn resumed_checkpoint_retains_exact_pending_copied_call() {
        let owner = owner();
        let ancestor_native_session_id = owner.parent_native_session_id.clone().unwrap();
        let pending_calls = BTreeMap::from([(
            "copied-call".to_owned(),
            CodexPendingCallV0 {
                raw_ordinal: 7,
                origin: CodexPendingCallOriginV0::CopiedFromAncestor {
                    ancestor_native_session_id,
                },
            },
        )]);
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: Some(&owner),
            local_turn_started: false,
            pending_calls: &pending_calls,
        })
        .unwrap()
        .unwrap();
        let decoded =
            CodexSemanticCheckpoint::decode_key(&checkpoint.encode_key().unwrap()).unwrap();

        assert!(decoded.direct_append_safe());
        assert_eq!(decoded.pending_calls(), &pending_calls);
    }
}
