use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::super::checkpoint::{
    CodexTerminalAuthorityCheckpointV0, MAX_CODEX_TERMINAL_AUTHORITIES,
};
use super::*;

const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/terminal-call-id/v1\0";

#[derive(Debug, Clone, Copy, Default)]
struct CodexTerminalAuthorityState {
    candidates: u8,
}

#[derive(Debug, Default)]
pub(super) struct CodexTerminalAuthority {
    prior_fingerprints: BTreeSet<u64>,
    call_ids: BTreeMap<[u8; 32], CodexTerminalAuthorityState>,
    current_fingerprints: BTreeMap<u64, [u8; 32]>,
    saturated: bool,
    append_resume: bool,
    replacement_required: bool,
}

impl CodexTerminalAuthority {
    pub(super) fn restore(&mut self, checkpoint: &CodexTerminalAuthorityCheckpointV0) -> bool {
        if self.append_resume
            || self.saturated
            || !self.prior_fingerprints.is_empty()
            || !self.call_ids.is_empty()
            || !self.current_fingerprints.is_empty()
            || self.replacement_required
        {
            return false;
        }
        self.append_resume = true;
        match checkpoint.fingerprints() {
            Some(fingerprints) => self.prior_fingerprints.extend(fingerprints.iter().copied()),
            None => self.saturated = true,
        }
        true
    }

    pub(super) fn observe_record(&mut self, record: &[u8]) {
        match classify_codex_record(record) {
            Ok(probe) if probe.lineage_malformed() => {
                if terminal_call_id(&probe).is_some()
                    || classify_after_selector_ambiguity(record)
                        .as_ref()
                        .and_then(terminal_call_id)
                        .is_some()
                {
                    // The bounded classifier deliberately retains only the
                    // first and last duplicate selectors. Once a terminal's
                    // linkage selectors are ambiguous, fail open for every
                    // result instead of guessing which hidden value owns it.
                    self.saturate();
                }
            }
            Ok(probe) => {
                if let Some(call_id) = terminal_call_id(&probe) {
                    self.observe_call_id(call_id);
                }
            }
            Err(_) => {
                if let Some(call_id) = classify_after_selector_ambiguity(record)
                    .as_ref()
                    .and_then(terminal_call_id)
                {
                    self.observe_call_id(call_id);
                }
            }
        }
    }

    pub(super) fn saturate(&mut self) {
        self.replacement_required |= self.append_resume;
        self.prior_fingerprints.clear();
        self.call_ids.clear();
        self.current_fingerprints.clear();
        self.saturated = true;
    }

    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        !self.saturated
            && !self.replacement_required
            && self
                .call_ids
                .get(&terminal_call_id_digest(call_id))
                .is_some_and(|state| state.candidates == 1)
    }

    pub(super) fn append_requires_replacement(&self) -> bool {
        self.replacement_required
    }

    pub(super) fn checkpoint(&self) -> CodexTerminalAuthorityCheckpointV0 {
        if self.saturated {
            return CodexTerminalAuthorityCheckpointV0::saturated();
        }
        let mut fingerprints = self.prior_fingerprints.clone();
        fingerprints.extend(self.current_fingerprints.keys().copied());
        CodexTerminalAuthorityCheckpointV0::exact(fingerprints.into_iter().collect())
    }

    fn observe_call_id(&mut self, call_id: &str) {
        if call_id.is_empty() || call_id.len() > super::super::checkpoint::MAX_CODEX_CALL_ID_BYTES {
            return;
        }
        if self.saturated {
            self.replacement_required |= self.append_resume;
            return;
        }
        let digest = terminal_call_id_digest(call_id);
        let fingerprint = terminal_call_id_fingerprint(&digest);
        if self.append_resume && self.prior_fingerprints.contains(&fingerprint) {
            // Fingerprint matches are deliberately conservative. Equality is
            // possible, so only a replacement scan may decide linkage.
            self.replacement_required = true;
            return;
        }
        if let Some(state) = self.call_ids.get_mut(&digest) {
            state.candidates = state.candidates.saturating_add(1).min(2);
            return;
        }
        if self
            .current_fingerprints
            .get(&fingerprint)
            .is_some_and(|existing| existing != &digest)
            || self
                .prior_fingerprints
                .len()
                .saturating_add(self.call_ids.len())
                >= MAX_CODEX_TERMINAL_AUTHORITIES
        {
            // A compact collision cannot be represented exactly, and the next
            // distinct ID would exceed the bounded authority. Both states
            // conservatively expose every result after a replacement scan.
            self.saturate();
            return;
        }
        self.current_fingerprints.insert(fingerprint, digest);
        self.call_ids
            .insert(digest, CodexTerminalAuthorityState { candidates: 1 });
    }
}

fn terminal_call_id<'a>(probe: &'a CodexRecordProbe<'_>) -> Option<&'a str> {
    matches!(probe.class, CodexRecordClass::ExcludedResult(_))
        .then_some(probe.call_id.as_deref())
        .flatten()
}

fn terminal_call_id_digest(call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TERMINAL_CALL_ID_DOMAIN);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

fn terminal_call_id_fingerprint(digest: &[u8; 32]) -> u64 {
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(call_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": "result"
            }
        }))
        .unwrap()
    }

    #[test]
    fn terminal_authority_detects_cold_and_cross_prefix_duplicates() {
        let mut cold = CodexTerminalAuthority::default();
        cold.observe_record(&terminal("duplicate"));
        cold.observe_record(&terminal("duplicate"));
        assert!(!cold.is_unique("duplicate"));
        assert!(!cold.append_requires_replacement());

        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(&terminal("duplicate"));
        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("duplicate"));
        assert!(appended.append_requires_replacement());
    }

    #[test]
    fn terminal_authority_keeps_unique_suffix_direct_across_restart() {
        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(&terminal("prefix"));

        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("suffix"));
        assert!(!appended.append_requires_replacement());
        assert!(appended.is_unique("suffix"));

        let mut restarted = CodexTerminalAuthority::default();
        assert!(restarted.restore(&appended.checkpoint()));
        restarted.observe_record(&terminal("after-restart"));
        assert!(!restarted.append_requires_replacement());
        assert!(restarted.is_unique("after-restart"));
    }

    #[test]
    fn terminal_authority_saturates_on_the_4097th_distinct_id() {
        let mut prefix = CodexTerminalAuthority::default();
        for index in 0..MAX_CODEX_TERMINAL_AUTHORITIES {
            prefix.observe_record(&terminal(&format!("terminal-{index}")));
        }
        assert!(prefix.checkpoint().fingerprints().is_some());

        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("terminal-overflow"));
        assert!(appended.append_requires_replacement());
        assert!(appended.checkpoint().fingerprints().is_none());
    }
}
