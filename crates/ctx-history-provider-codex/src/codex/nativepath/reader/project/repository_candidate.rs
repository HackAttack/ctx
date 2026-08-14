use std::{collections::BTreeMap, mem::size_of};

use super::*;

const REPOSITORY_CANDIDATE_AUTHORITY_ENTRY_OVERHEAD_BYTES: usize = 3 * size_of::<usize>();
const MAX_CODEX_REPOSITORY_OCCURRENCE_CACHE_ENTRIES: usize = 16_384;

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

#[derive(Debug, Clone, Default)]
pub(in super::super) struct CodexRepositoryOccurrenceCache {
    entries: BTreeMap<[u8; 32], RepositoryCandidateAuthorityState>,
    exhausted: bool,
    peak_entries: usize,
    negative_authority: super::super::super::checkpoint::CodexRepositoryOccurrenceNegativeAuthority,
}

impl CodexRepositoryOccurrenceCache {
    pub(in super::super) fn from_negative_authority(
        authority: super::super::super::checkpoint::CodexRepositoryOccurrenceNegativeAuthority,
    ) -> Self {
        Self {
            negative_authority: authority,
            ..Self::default()
        }
    }

    pub(in super::super) fn negative_authority(
        &self,
    ) -> super::super::super::checkpoint::CodexRepositoryOccurrenceNegativeAuthority {
        self.negative_authority.clone()
    }

    pub(in super::super) fn merge_negative_authority(&mut self, suffix: &Self) {
        self.negative_authority.merge(&suffix.negative_authority);
    }

    pub(in super::super) fn observe_call(&mut self, call_id: &str) {
        self.observe(call_id, true);
    }

    pub(in super::super) fn observe_result(&mut self, call_id: &str) {
        self.observe(call_id, false);
    }

    pub(in super::super) fn observe_ambiguous_record(&mut self) {
        self.negative_authority.mark_incomplete();
        self.exhaust();
    }

    pub(in super::super) fn apply_suffix_to(
        &self,
        authority: &mut CodexRepositoryCandidateAuthority,
    ) {
        if self.exhausted {
            authority.exhaust();
            return;
        }
        for (digest, candidate) in &mut authority.entries {
            let Some(observed) = self.entries.get(digest) else {
                continue;
            };
            candidate.calls = candidate.calls.saturating_add(observed.calls).min(2);
            candidate.results = candidate.results.saturating_add(observed.results).min(2);
        }
    }

    pub(in super::super) fn peak_entry_count(&self) -> usize {
        self.peak_entries
    }

    pub(in super::super) fn estimated_peak_owned_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.peak_entries.saturating_mul(
                size_of::<([u8; 32], RepositoryCandidateAuthorityState)>()
                    .saturating_add(REPOSITORY_CANDIDATE_AUTHORITY_ENTRY_OVERHEAD_BYTES),
            ),
        )
    }

    fn observe(&mut self, call_id: &str, call: bool) {
        let digest = repository_candidate_call_id_digest(call_id);
        self.negative_authority.observe(&digest);
        if self.exhausted {
            return;
        }
        let new_entry = !self.entries.contains_key(&digest);
        if new_entry && self.entries.len() >= MAX_CODEX_REPOSITORY_OCCURRENCE_CACHE_ENTRIES {
            self.exhaust();
            return;
        }
        self.peak_entries = self
            .peak_entries
            .max(self.entries.len().saturating_add(usize::from(new_entry)));
        let state = self.entries.entry(digest).or_default();
        if call {
            state.calls = state.calls.saturating_add(1).min(2);
        } else {
            state.results = state.results.saturating_add(1).min(2);
        }
    }

    fn exhaust(&mut self) {
        self.entries.clear();
        self.exhausted = true;
    }
}

impl CodexRepositoryCandidateAuthority {
    pub(in super::super) fn from_checkpoint(
        checkpoint: &super::super::super::checkpoint::CodexRepositoryAuthorityCheckpoint,
    ) -> Self {
        Self {
            entries: checkpoint
                .candidates
                .iter()
                .map(|entry| {
                    (
                        entry.digest,
                        RepositoryCandidateAuthorityState {
                            calls: entry.multiplicity.calls,
                            results: entry.multiplicity.results,
                        },
                    )
                })
                .collect(),
            exhausted: checkpoint.candidate_exhausted,
        }
    }

    pub(in super::super) fn checkpoint_entries(
        &self,
    ) -> Vec<super::super::super::checkpoint::CodexRepositoryAuthorityEntryCheckpoint> {
        self.entries
            .iter()
            .map(|(digest, state)| checkpoint_entry(*digest, *state))
            .collect()
    }

    pub(in super::super) fn is_exhausted(&self) -> bool {
        self.exhausted
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

    pub(in super::super) fn newly_admitted_candidate_hits_prefix(
        &self,
        combined: &CodexRepositoryCandidateAuthority,
        prefix: &super::super::super::checkpoint::CodexRepositoryOccurrenceNegativeAuthority,
    ) -> bool {
        !combined.exhausted
            && combined.entries.keys().any(|digest| {
                !self.entries.contains_key(digest) && !prefix.definitely_absent(digest)
            })
    }

    #[cfg(test)]
    pub(in super::super) fn observe_candidate_call(&mut self, call_id: &str) {
        self.admit_candidate(call_id);
        self.observe_call_if_candidate(call_id);
    }

    pub(in super::super) fn admit_candidate(&mut self, call_id: &str) {
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
        self.entries.entry(digest).or_default();
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    pub(in super::super) fn contains_candidate(&self, call_id: &str) -> bool {
        self.entries
            .contains_key(&repository_candidate_call_id_digest(call_id))
    }

    #[cfg(test)]
    pub(in super::super) fn exhausted(&self) -> bool {
        self.exhausted
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

fn checkpoint_entry(
    digest: [u8; 32],
    state: RepositoryCandidateAuthorityState,
) -> super::super::super::checkpoint::CodexRepositoryAuthorityEntryCheckpoint {
    use super::super::super::checkpoint::{
        CodexRepositoryAuthorityEntryCheckpoint, CodexRepositoryMultiplicityCheckpoint,
    };

    CodexRepositoryAuthorityEntryCheckpoint {
        digest,
        multiplicity: CodexRepositoryMultiplicityCheckpoint {
            calls: state.calls,
            results: state.results,
        },
    }
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
    fn duplicate_result_serial_call_and_overflow_abstain() {
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
        assert!(!overflow.is_unique_call_and_result("candidate-0"));
    }

    #[test]
    fn occurrence_cache_counts_before_admission_and_overflow_abstains() {
        let mut cache = CodexRepositoryOccurrenceCache::default();
        for index in 0..10_000 {
            cache.observe_result(&format!("unrelated-{index}"));
        }
        cache.observe_result("late-candidate");
        cache.observe_call("late-candidate");
        let mut authority = CodexRepositoryCandidateAuthority::default();
        authority.admit_candidate("late-candidate");
        cache.apply_suffix_to(&mut authority);
        assert!(authority.is_unique_call_and_result("late-candidate"));
        assert_eq!(cache.peak_entry_count(), 10_001);

        let mut overflow = CodexRepositoryOccurrenceCache::default();
        for index in 0..=MAX_CODEX_REPOSITORY_OCCURRENCE_CACHE_ENTRIES {
            overflow.observe_result(&format!("unrelated-{index}"));
        }
        overflow.apply_suffix_to(&mut authority);
        assert!(authority.exhausted());
    }

    #[test]
    fn prefix_adaptive_authority_hits_retry_and_misses_resume() {
        let mut exact_prefix_occurrences = CodexRepositoryOccurrenceCache::default();
        exact_prefix_occurrences.observe_result("small-prefix-occurrence");
        let exact_negative = exact_prefix_occurrences.negative_authority();
        let prefix_candidates = CodexRepositoryCandidateAuthority::default();

        let mut exact_observed = CodexRepositoryCandidateAuthority::default();
        exact_observed.admit_candidate("small-prefix-occurrence");
        assert!(prefix_candidates
            .newly_admitted_candidate_hits_prefix(&exact_observed, &exact_negative));
        let mut exact_absent = CodexRepositoryCandidateAuthority::default();
        exact_absent.admit_candidate("small-definitely-absent");
        assert!(
            !prefix_candidates.newly_admitted_candidate_hits_prefix(&exact_absent, &exact_negative)
        );

        let mut prefix_occurrences = CodexRepositoryOccurrenceCache::default();
        for index in 0..MAX_CODEX_REPOSITORY_OCCURRENCE_CACHE_ENTRIES {
            prefix_occurrences.observe_result(&format!("prefix-occurrence-{index}"));
        }
        let negative = prefix_occurrences.negative_authority();

        let mut observed = CodexRepositoryCandidateAuthority::default();
        observed.admit_candidate("prefix-occurrence-0");
        assert!(prefix_candidates.newly_admitted_candidate_hits_prefix(&observed, &negative));

        let collision = (0..100_000)
            .map(|index| format!("unobserved-collision-candidate-{index}"))
            .find(|call_id| {
                !negative.definitely_absent(&repository_candidate_call_id_digest(call_id))
            })
            .expect("32 KiB Bloom has a deterministic false-positive fixture");
        let mut colliding = CodexRepositoryCandidateAuthority::default();
        colliding.admit_candidate(&collision);
        assert!(prefix_candidates.newly_admitted_candidate_hits_prefix(&colliding, &negative));

        let absent = (0..100)
            .map(|index| format!("definitely-absent-candidate-{index}"))
            .find(|call_id| {
                negative.definitely_absent(&repository_candidate_call_id_digest(call_id))
            })
            .expect("a Bloom miss is readily available");
        let mut resumable = CodexRepositoryCandidateAuthority::default();
        resumable.admit_candidate(&absent);
        assert!(!prefix_candidates.newly_admitted_candidate_hits_prefix(&resumable, &negative));
    }
}
