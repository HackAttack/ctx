use ctx_history_core::TypedKey;
use serde::{Deserialize, Serialize};

use super::rows::CodexSessionRow;

pub(super) const MAX_CODEX_SEMANTIC_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
const CODEX_SEMANTIC_CHECKPOINT_VERSION: u8 = 4;
const CODEX_SEMANTIC_CHECKPOINT_PREFIX: &str = "codex.projector-checkpoint.v4:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexSemanticCheckpoint {
    version: u8,
    owner: Option<CodexSessionRow>,
    local_turn_started: bool,
}

pub(super) struct CodexSemanticCheckpointState<'a> {
    pub(super) owner: Option<&'a CodexSessionRow>,
    pub(super) local_turn_started: bool,
}

impl CodexSemanticCheckpoint {
    pub(super) fn from_state(
        state: CodexSemanticCheckpointState<'_>,
    ) -> Result<Option<Self>, serde_json::Error> {
        let checkpoint = Self {
            version: CODEX_SEMANTIC_CHECKPOINT_VERSION,
            owner: state.owner.cloned(),
            local_turn_started: state.local_turn_started,
        };
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
        if checkpoint.version != CODEX_SEMANTIC_CHECKPOINT_VERSION {
            return Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Codex semantic checkpoint version is invalid",
            )));
        }
        Ok(checkpoint)
    }

    pub(super) fn direct_append_safe(&self) -> bool {
        self.version == CODEX_SEMANTIC_CHECKPOINT_VERSION && self.owner.is_some()
    }

    pub(super) fn owner(&self) -> Option<&CodexSessionRow> {
        self.owner.as_ref()
    }

    pub(super) const fn local_turn_started(&self) -> bool {
        self.local_turn_started
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

    #[test]
    fn neutral_checkpoint_round_trip_contains_only_scanner_continuation() {
        let checkpoint = CodexSemanticCheckpoint::from_state(CodexSemanticCheckpointState {
            owner: None,
            local_turn_started: false,
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
}
