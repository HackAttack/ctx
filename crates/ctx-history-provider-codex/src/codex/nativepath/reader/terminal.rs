use sha2::{Digest, Sha256};

use super::*;

const MAX_CODEX_TERMINAL_AUTHORITIES: usize = 4 * 1024;
const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/terminal-call-id/v1\0";

#[derive(Debug, Clone, Copy, Default)]
struct CodexTerminalAuthorityState {
    candidates: u8,
    in_certified_prefix: bool,
    after_certified_prefix: bool,
}

#[derive(Debug, Default)]
pub(super) struct CodexTerminalAuthority {
    call_ids: BTreeMap<[u8; 32], CodexTerminalAuthorityState>,
    exhausted: bool,
    exhausted_after_certified_prefix: bool,
}

impl CodexTerminalAuthority {
    pub(super) fn observe_record(&mut self, record: &[u8], in_certified_prefix: bool) {
        if self.exhausted {
            return;
        }
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
                    self.exhaust(in_certified_prefix);
                }
            }
            Ok(probe) => {
                if let Some(call_id) = terminal_call_id(&probe) {
                    self.observe_call_id(call_id, in_certified_prefix);
                }
            }
            Err(_) => {
                if let Some(call_id) = classify_after_selector_ambiguity(record)
                    .as_ref()
                    .and_then(terminal_call_id)
                {
                    self.observe_call_id(call_id, in_certified_prefix);
                }
            }
        }
    }

    pub(super) fn exhaust(&mut self, in_certified_prefix: bool) {
        if self.exhausted {
            return;
        }
        self.call_ids.clear();
        self.exhausted = true;
        self.exhausted_after_certified_prefix = !in_certified_prefix;
    }

    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        !self.exhausted
            && self
                .call_ids
                .get(&terminal_call_id_digest(call_id))
                .is_some_and(|state| state.candidates == 1)
    }

    pub(super) fn append_requires_replacement(&self) -> bool {
        self.exhausted_after_certified_prefix
            || self.call_ids.values().any(|state| {
                state.in_certified_prefix && state.after_certified_prefix && state.candidates > 1
            })
    }

    fn observe_call_id(&mut self, call_id: &str, in_certified_prefix: bool) {
        if call_id.is_empty() || call_id.len() > super::super::checkpoint::MAX_CODEX_CALL_ID_BYTES {
            return;
        }
        let digest = terminal_call_id_digest(call_id);
        if !self.call_ids.contains_key(&digest)
            && self.call_ids.len() >= MAX_CODEX_TERMINAL_AUTHORITIES
        {
            self.exhaust(in_certified_prefix);
            return;
        }
        let state = self.call_ids.entry(digest).or_default();
        state.candidates = state.candidates.saturating_add(1).min(2);
        state.in_certified_prefix |= in_certified_prefix;
        state.after_certified_prefix |= !in_certified_prefix;
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
        cold.observe_record(&terminal("duplicate"), false);
        cold.observe_record(&terminal("duplicate"), false);
        assert!(!cold.is_unique("duplicate"));
        assert!(!cold.append_requires_replacement());

        let mut appended = CodexTerminalAuthority::default();
        appended.observe_record(&terminal("duplicate"), true);
        appended.observe_record(&terminal("duplicate"), false);
        assert!(!appended.is_unique("duplicate"));
        assert!(appended.append_requires_replacement());
    }
}
