use std::collections::HashMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use ctx_history_provider_runtime::{CaptureError, Result};

use crate::mux::normalization::mux_history_sequence;

use super::super::{MuxStreamKind, MAX_EVENT_SEQUENCE_ORDINAL};

const ARCHIVE_ROW_EQUIVALENCE_DOMAIN: &[u8] = b"ctx.mux.archive-row-equivalence.v1\0";
const MAX_ARCHIVE_SEAM_EVIDENCE_OCCURRENCES: u64 = MAX_EVENT_SEQUENCE_ORDINAL + 1;

#[derive(Debug, Hash, PartialEq, Eq)]
struct ArchiveRowEvidence {
    provider_identity: Option<String>,
    history_sequence: Option<u64>,
    canonical_record_sha256: [u8; 32],
}

impl ArchiveRowEvidence {
    fn from_value(value: &Value) -> Result<Option<Self>> {
        let provider_identity = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|identity| !identity.trim().is_empty())
            .map(str::to_owned);
        let history_sequence = mux_history_sequence(value);
        if provider_identity.is_none() && history_sequence.is_none() {
            // Exact content alone cannot distinguish a replay from two
            // legitimately repeated rows. The fallback identity allocator can
            // keep such rows distinct, so do not suppress without native proof.
            return Ok(None);
        }
        let mut digest = Sha256::new();
        digest.update(ARCHIVE_ROW_EQUIVALENCE_DOMAIN);
        digest.update(serde_json::to_vec(value)?);
        Ok(Some(Self {
            provider_identity,
            history_sequence,
            canonical_record_sha256: digest.finalize().into(),
        }))
    }
}

pub(super) struct MuxArchiveSeam {
    archived_rows: HashMap<ArchiveRowEvidence, u64>,
    archived_evidence_occurrences: u64,
    chat_prefix_open: bool,
}

impl MuxArchiveSeam {
    pub(super) fn new() -> Self {
        Self {
            archived_rows: HashMap::new(),
            archived_evidence_occurrences: 0,
            chat_prefix_open: true,
        }
    }

    pub(super) fn suppress_replayed_chat_row(
        &mut self,
        stream: MuxStreamKind,
        value: &Value,
    ) -> Result<bool> {
        let evidence = ArchiveRowEvidence::from_value(value)?;
        if stream == MuxStreamKind::Archive {
            if let Some(evidence) = evidence {
                if self.archived_evidence_occurrences == MAX_ARCHIVE_SEAM_EVIDENCE_OCCURRENCES {
                    return Err(CaptureError::InvalidPayload(
                        "Mux archive seam evidence exceeds event identity capacity".to_owned(),
                    ));
                }
                *self.archived_rows.entry(evidence).or_default() += 1;
                self.archived_evidence_occurrences += 1;
            }
            return Ok(false);
        }
        if stream != MuxStreamKind::Chat || !self.chat_prefix_open {
            return Ok(false);
        }
        if let Some(evidence) = evidence {
            let mut consumed = false;
            let mut exhausted = false;
            if let Some(occurrences) = self.archived_rows.get_mut(&evidence) {
                *occurrences -= 1;
                consumed = true;
                exhausted = *occurrences == 0;
            }
            if consumed {
                self.archived_evidence_occurrences -= 1;
                if exhausted {
                    self.archived_rows.remove(&evidence);
                }
                return Ok(true);
            }
        }

        // Mux crash replay is a contiguous prefix of chat.jsonl. Once a row is
        // not equivalent to an archived candidate, later rows cannot be seam
        // overlap and must remain visible even if their sequence is covered.
        self.chat_prefix_open = false;
        Ok(false)
    }
}
