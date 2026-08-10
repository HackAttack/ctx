use std::{collections::BTreeMap, mem::size_of};

use super::*;

const REPOSITORY_CANDIDATE_AUTHORITY_ENTRY_OVERHEAD_BYTES: usize = 3 * size_of::<usize>();

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RepositoryCandidateAuthorityState {
    calls: u8,
    results: u8,
}

#[derive(Debug, Clone, Default)]
pub(in super::super) struct CodexRepositoryCandidateAuthority {
    entries: BTreeMap<[u8; 32], RepositoryCandidateAuthorityState>,
    exhausted: bool,
}

impl CodexRepositoryCandidateAuthority {
    pub(in super::super) fn from_checkpoint(
        checkpoint: &CodexRepositoryCandidateAuthorityCheckpoint,
    ) -> Self {
        Self {
            entries: checkpoint
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.call_id_sha256,
                        RepositoryCandidateAuthorityState {
                            calls: entry.calls,
                            results: entry.results,
                        },
                    )
                })
                .collect(),
            exhausted: checkpoint.exhausted,
        }
    }

    pub(in super::super) fn checkpoint(&self) -> CodexRepositoryCandidateAuthorityCheckpoint {
        CodexRepositoryCandidateAuthorityCheckpoint {
            entries: self
                .entries
                .iter()
                .map(
                    |(call_id_sha256, state)| CodexRepositoryCandidateAuthorityEntry {
                        call_id_sha256: *call_id_sha256,
                        calls: state.calls,
                        results: state.results,
                    },
                )
                .collect(),
            exhausted: self.exhausted,
        }
    }

    pub(in super::super) fn appended_suffix_invalidates(
        &self,
        combined: &CodexRepositoryCandidateAuthority,
    ) -> bool {
        if self.exhausted {
            return false;
        }
        self.entries.iter().any(|(digest, state)| {
            state.calls == 1
                && state.results == 1
                && (combined.exhausted
                    || combined
                        .entries
                        .get(digest)
                        .is_none_or(|combined| combined.calls != 1 || combined.results != 1))
        })
    }

    pub(in super::super) fn observe_candidate_call(&mut self, call_id: &str) {
        if self.exhausted {
            return;
        }
        let digest = repository_candidate_call_id_digest(call_id);
        if !self.entries.contains_key(&digest)
            && self.entries.len() >= MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES
        {
            self.exhaust();
            return;
        }
        let state = self.entries.entry(digest).or_default();
        state.calls = state.calls.saturating_add(1).min(2);
    }

    pub(in super::super) fn observe_call_if_candidate(&mut self, call_id: &str) {
        if self.exhausted {
            return;
        }
        if let Some(state) = self
            .entries
            .get_mut(&repository_candidate_call_id_digest(call_id))
        {
            state.calls = state.calls.saturating_add(1).min(2);
        }
    }

    pub(in super::super) fn observe_result_if_candidate(&mut self, call_id: &str) -> bool {
        if self.exhausted {
            return false;
        }
        let Some(state) = self
            .entries
            .get_mut(&repository_candidate_call_id_digest(call_id))
        else {
            return false;
        };
        state.results = state.results.saturating_add(1).min(2);
        true
    }

    pub(in super::super) fn observe_ambiguous_record(&mut self) {
        self.exhaust();
    }

    pub(super) fn proves_exact_outcome(
        &self,
        context: &CodexToolCallContext,
        result_call_id: &str,
    ) -> bool {
        let Some(origin_call_id) = context.origin_call_id.as_deref() else {
            return false;
        };
        self.is_unique_call_and_result(origin_call_id)
            && self.is_unique_call_and_result(result_call_id)
            && context
                .continuation_call_id_sha256
                .iter()
                .all(|digest| self.is_unique_digest(digest))
    }

    pub(in super::super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(in super::super) fn estimated_owned_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.entries.len().saturating_mul(
                size_of::<([u8; 32], RepositoryCandidateAuthorityState)>()
                    .saturating_add(REPOSITORY_CANDIDATE_AUTHORITY_ENTRY_OVERHEAD_BYTES),
            ),
        )
    }

    fn is_unique_call_and_result(&self, call_id: &str) -> bool {
        self.is_unique_digest(&repository_candidate_call_id_digest(call_id))
    }

    fn is_unique_digest(&self, digest: &[u8; 32]) -> bool {
        !self.exhausted
            && self
                .entries
                .get(digest)
                .is_some_and(|state| state.calls == 1 && state.results == 1)
    }

    fn exhaust(&mut self) {
        self.entries.clear();
        self.exhausted = true;
    }
}

fn repository_candidate_call_id_digest(call_id: &str) -> [u8; 32] {
    crate::provider::codex::repository::continuation_call_id_sha256(call_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_results_do_not_consume_candidate_capacity() {
        let mut authority = CodexRepositoryCandidateAuthority::default();
        for index in 0..10_000 {
            assert!(!authority.observe_result_if_candidate(&format!("unrelated-{index}")));
        }
        authority.observe_candidate_call("candidate");
        assert!(authority.observe_result_if_candidate("candidate"));
        assert_eq!(authority.entry_count(), 1);
        assert!(authority.proves_exact_outcome(
            &CodexToolCallContext {
                origin_call_id: Some("candidate".to_owned()),
                ..CodexToolCallContext::default()
            },
            "candidate"
        ));
    }

    #[test]
    fn duplicate_result_serial_call_and_overflow_abstain_durably() {
        let mut duplicate = CodexRepositoryCandidateAuthority::default();
        duplicate.observe_candidate_call("duplicate");
        duplicate.observe_result_if_candidate("duplicate");
        duplicate.observe_result_if_candidate("duplicate");
        assert!(!duplicate.is_unique_call_and_result("duplicate"));

        let mut serial = CodexRepositoryCandidateAuthority::default();
        serial.observe_candidate_call("serial");
        serial.observe_result_if_candidate("serial");
        serial.observe_call_if_candidate("serial");
        assert!(!serial.is_unique_call_and_result("serial"));

        let mut overflow = CodexRepositoryCandidateAuthority::default();
        for index in 0..=MAX_CODEX_REPOSITORY_CANDIDATE_AUTHORITIES {
            let call_id = format!("candidate-{index}");
            overflow.observe_candidate_call(&call_id);
            overflow.observe_result_if_candidate(&call_id);
        }
        assert!(overflow.exhausted);
        let restarted = CodexRepositoryCandidateAuthority::from_checkpoint(&overflow.checkpoint());
        assert!(!restarted.is_unique_call_and_result("candidate-0"));
    }
}
