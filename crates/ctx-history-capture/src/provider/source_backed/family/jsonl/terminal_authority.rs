use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlTerminalObservationRegion {
    WholeSource,
    CertifiedPrefix,
    AppendedSuffix,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct JsonlTerminalMultiplicity {
    candidates: u8,
    in_certified_prefix: bool,
    in_appended_suffix: bool,
}

pub(crate) trait JsonlTerminalStorage: Clone + std::fmt::Debug + Default {
    type Iter<'a>: Iterator<Item = (&'a [u8; 32], &'a JsonlTerminalMultiplicity)>
    where
        Self: 'a;

    fn len(&self) -> usize;
    fn contains_key(&self, digest: &[u8; 32]) -> bool;
    fn entry_or_default(&mut self, digest: [u8; 32]) -> &mut JsonlTerminalMultiplicity;
    fn get(&self, digest: &[u8; 32]) -> Option<&JsonlTerminalMultiplicity>;
    fn clear(&mut self);
    fn iter(&self) -> Self::Iter<'_>;
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlHashTerminalStorage(HashMap<[u8; 32], JsonlTerminalMultiplicity>);

impl JsonlTerminalStorage for JsonlHashTerminalStorage {
    type Iter<'a> = std::collections::hash_map::Iter<'a, [u8; 32], JsonlTerminalMultiplicity>;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn contains_key(&self, digest: &[u8; 32]) -> bool {
        self.0.contains_key(digest)
    }

    fn entry_or_default(&mut self, digest: [u8; 32]) -> &mut JsonlTerminalMultiplicity {
        self.0.entry(digest).or_default()
    }

    fn get(&self, digest: &[u8; 32]) -> Option<&JsonlTerminalMultiplicity> {
        self.0.get(digest)
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlOrderedTerminalStorage(BTreeMap<[u8; 32], JsonlTerminalMultiplicity>);

impl JsonlTerminalStorage for JsonlOrderedTerminalStorage {
    type Iter<'a> = std::collections::btree_map::Iter<'a, [u8; 32], JsonlTerminalMultiplicity>;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn contains_key(&self, digest: &[u8; 32]) -> bool {
        self.0.contains_key(digest)
    }

    fn entry_or_default(&mut self, digest: [u8; 32]) -> &mut JsonlTerminalMultiplicity {
        self.0.entry(digest).or_default()
    }

    fn get(&self, digest: &[u8; 32]) -> Option<&JsonlTerminalMultiplicity> {
        self.0.get(digest)
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlTerminalAuthorityMap<S: JsonlTerminalStorage> {
    call_ids: S,
    exhausted: bool,
    available: bool,
}

pub(crate) type JsonlTerminalAuthority = JsonlTerminalAuthorityMap<JsonlHashTerminalStorage>;
pub(crate) type JsonlCheckpointedTerminalAuthority =
    JsonlTerminalAuthorityMap<JsonlOrderedTerminalStorage>;

impl<S: JsonlTerminalStorage> JsonlTerminalAuthorityMap<S> {
    pub(crate) fn available() -> Self {
        Self {
            available: true,
            ..Self::default()
        }
    }

    pub(crate) fn observe(
        &mut self,
        domain: &[u8],
        call_id: &str,
        region: JsonlTerminalObservationRegion,
        capacity: usize,
    ) {
        self.observe_digest(
            jsonl_terminal_call_id_digest(domain, call_id),
            region,
            capacity,
        );
    }

    pub(crate) fn observe_digest(
        &mut self,
        digest: [u8; 32],
        region: JsonlTerminalObservationRegion,
        capacity: usize,
    ) {
        self.available = true;
        if self.exhausted {
            return;
        }
        if !self.call_ids.contains_key(&digest) && self.call_ids.len() >= capacity {
            self.observe_ambiguous_terminal();
            return;
        }
        let state = self.call_ids.entry_or_default(digest);
        state.candidates = state.candidates.saturating_add(1).min(2);
        match region {
            JsonlTerminalObservationRegion::WholeSource => {}
            JsonlTerminalObservationRegion::CertifiedPrefix => {
                state.in_certified_prefix = true;
            }
            JsonlTerminalObservationRegion::AppendedSuffix => {
                state.in_appended_suffix = true;
            }
        }
    }

    pub(crate) fn is_unique(&self, domain: &[u8], call_id: &str) -> bool {
        self.is_unique_digest(jsonl_terminal_call_id_digest(domain, call_id))
    }

    pub(crate) fn is_unique_digest(&self, digest: [u8; 32]) -> bool {
        !self.available
            || (!self.exhausted
                && self
                    .call_ids
                    .get(&digest)
                    .is_some_and(|state| state.candidates == 1))
    }

    pub(crate) fn append_requires_replacement(&self) -> bool {
        self.exhausted
            || self.call_ids.iter().any(|(_, state)| {
                state.in_certified_prefix && state.in_appended_suffix && state.candidates > 1
            })
    }

    pub(crate) fn observe_ambiguous_terminal(&mut self) {
        self.available = true;
        self.call_ids.clear();
        self.exhausted = true;
    }

    pub(crate) fn from_digest_counts(
        entries: impl IntoIterator<Item = ([u8; 32], u8)>,
        exhausted: bool,
    ) -> Self {
        let mut authority = Self {
            call_ids: S::default(),
            exhausted,
            available: true,
        };
        for (digest, candidates) in entries {
            authority.call_ids.entry_or_default(digest).candidates = candidates;
        }
        authority
    }

    pub(crate) fn digest_counts(&self) -> impl Iterator<Item = ([u8; 32], u8)> + '_ {
        self.call_ids
            .iter()
            .map(|(digest, state)| (*digest, state.candidates))
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(crate) fn positive_claim_invalidated_by(&self, combined: &Self) -> bool {
        if !self.available || self.exhausted {
            return false;
        }
        self.call_ids.iter().any(|(digest, state)| {
            state.candidates == 1
                && (combined.exhausted
                    || combined
                        .call_ids
                        .get(digest)
                        .is_none_or(|combined| combined.candidates != 1))
        })
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.call_ids.len()
    }
}

#[inline]
pub(crate) fn jsonl_terminal_call_id_digest(domain: &[u8], call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN: &[u8] = b"ctx/test/jsonl-terminal-authority/v1\0";

    #[test]
    fn exact_multiplicity_and_capacity_fail_closed() {
        let mut authority = JsonlTerminalAuthority::available();
        authority.observe(
            DOMAIN,
            "one",
            JsonlTerminalObservationRegion::WholeSource,
            2,
        );
        assert!(authority.is_unique(DOMAIN, "one"));
        authority.observe(
            DOMAIN,
            "one",
            JsonlTerminalObservationRegion::WholeSource,
            2,
        );
        assert!(!authority.is_unique(DOMAIN, "one"));
        authority.observe(
            DOMAIN,
            "two",
            JsonlTerminalObservationRegion::WholeSource,
            2,
        );
        authority.observe(
            DOMAIN,
            "three",
            JsonlTerminalObservationRegion::WholeSource,
            2,
        );
        assert!(!authority.is_unique(DOMAIN, "two"));
    }

    #[test]
    fn duplicate_crossing_the_certified_boundary_requires_replacement() {
        let mut authority = JsonlTerminalAuthority::available();
        authority.observe(
            DOMAIN,
            "call",
            JsonlTerminalObservationRegion::CertifiedPrefix,
            8,
        );
        assert!(!authority.append_requires_replacement());
        authority.observe(
            DOMAIN,
            "call",
            JsonlTerminalObservationRegion::AppendedSuffix,
            8,
        );
        assert!(authority.append_requires_replacement());
    }

    #[test]
    fn unavailable_authority_preserves_legacy_fail_open_behavior() {
        assert!(JsonlTerminalAuthority::default().is_unique(DOMAIN, "unknown"));
    }

    #[test]
    fn checkpointed_digest_counts_preserve_positive_claims_and_invalidation() {
        let digest = jsonl_terminal_call_id_digest(DOMAIN, "call");
        let prefix = JsonlCheckpointedTerminalAuthority::from_digest_counts([(digest, 1)], false);
        assert_eq!(
            prefix.digest_counts().collect::<Vec<_>>(),
            vec![(digest, 1)]
        );
        assert!(prefix.is_unique(DOMAIN, "call"));

        let mut combined = prefix.clone();
        combined.observe_digest(digest, JsonlTerminalObservationRegion::WholeSource, 8);
        assert!(prefix.positive_claim_invalidated_by(&combined));
    }
}
