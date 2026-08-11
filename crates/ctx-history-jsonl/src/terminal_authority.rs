use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlTerminalObservationRegion {
    WholeSource,
    CertifiedPrefix,
    AppendedSuffix,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JsonlTerminalMultiplicity(u8);

const TERMINAL_CANDIDATES_MASK: u8 = 0b0000_0011;

pub trait JsonlTerminalStorage: Clone + std::fmt::Debug + Default {
    type Iter<'a>: Iterator<Item = (&'a [u8; 32], &'a JsonlTerminalMultiplicity)>
    where
        Self: 'a;

    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn contains_key(&self, digest: &[u8; 32]) -> bool;
    fn entry_or_default(&mut self, digest: [u8; 32]) -> &mut JsonlTerminalMultiplicity;
    fn get(&self, digest: &[u8; 32]) -> Option<&JsonlTerminalMultiplicity>;
    fn clear(&mut self);
    fn iter(&self) -> Self::Iter<'_>;
}

#[derive(Debug, Clone, Default)]
pub struct JsonlHashTerminalStorage(HashMap<[u8; 32], JsonlTerminalMultiplicity>);

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
pub struct JsonlOrderedTerminalStorage(BTreeMap<[u8; 32], JsonlTerminalMultiplicity>);

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
pub struct JsonlTerminalAuthorityMap<S: JsonlTerminalStorage> {
    call_ids: S,
    exhausted: bool,
    available: bool,
}

pub type JsonlTerminalAuthority = JsonlTerminalAuthorityMap<JsonlHashTerminalStorage>;
pub type JsonlCheckpointedTerminalAuthority =
    JsonlTerminalAuthorityMap<JsonlOrderedTerminalStorage>;

impl<S: JsonlTerminalStorage> JsonlTerminalAuthorityMap<S> {
    pub fn available() -> Self {
        Self {
            available: true,
            ..Self::default()
        }
    }

    #[inline]
    pub fn observe(
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

    #[inline]
    pub fn observe_digest(
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
        let candidates = (state.0 & TERMINAL_CANDIDATES_MASK)
            .saturating_add(1)
            .min(2);
        let region_flags = (u8::from(region == JsonlTerminalObservationRegion::CertifiedPrefix)
            << 2)
            | (u8::from(region == JsonlTerminalObservationRegion::AppendedSuffix) << 3);
        state.0 = (state.0 & !TERMINAL_CANDIDATES_MASK) | candidates | region_flags;
    }

    #[inline]
    pub fn is_unique(&self, domain: &[u8], call_id: &str) -> bool {
        !self.available
            || (!self.exhausted
                && self.is_unique_digest(jsonl_terminal_call_id_digest(domain, call_id)))
    }

    #[inline]
    pub fn is_unique_digest(&self, digest: [u8; 32]) -> bool {
        !self.available
            || (!self.exhausted
                && self
                    .call_ids
                    .get(&digest)
                    .is_some_and(|state| state.0 & TERMINAL_CANDIDATES_MASK == 1))
    }

    pub fn append_requires_replacement(&self) -> bool {
        self.exhausted
            || self.call_ids.iter().any(|(_, state)| {
                state.0 & (1 << 2) != 0
                    && state.0 & (1 << 3) != 0
                    && state.0 & TERMINAL_CANDIDATES_MASK > 1
            })
    }

    pub fn ambiguity_fingerprint(&self, domain: &[u8]) -> [u8; 32] {
        let mut ambiguous = self
            .call_ids
            .iter()
            .filter_map(|(digest, state)| {
                (state.0 & TERMINAL_CANDIDATES_MASK > 1).then_some(*digest)
            })
            .collect::<Vec<_>>();
        ambiguous.sort_unstable();
        let mut hasher = Sha256::new();
        hasher.update(domain);
        hasher.update([u8::from(self.exhausted)]);
        for digest in ambiguous {
            hasher.update(digest);
        }
        hasher.finalize().into()
    }

    pub fn observe_ambiguous_terminal(&mut self) {
        self.available = true;
        self.call_ids.clear();
        self.exhausted = true;
    }

    pub fn from_digest_counts(
        entries: impl IntoIterator<Item = ([u8; 32], u8)>,
        exhausted: bool,
    ) -> Self {
        let mut authority = Self {
            call_ids: S::default(),
            exhausted,
            available: true,
        };
        for (digest, candidates) in entries {
            debug_assert!(candidates <= 2);
            authority.call_ids.entry_or_default(digest).0 = candidates;
        }
        authority
    }

    pub fn digest_counts(&self) -> impl Iterator<Item = ([u8; 32], u8)> + '_ {
        self.call_ids
            .iter()
            .map(|(digest, state)| (*digest, state.0 & TERMINAL_CANDIDATES_MASK))
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn positive_claim_invalidated_by(&self, combined: &Self) -> bool {
        if !self.available || self.exhausted {
            return false;
        }
        self.call_ids.iter().any(|(digest, state)| {
            state.0 & TERMINAL_CANDIDATES_MASK == 1
                && (combined.exhausted
                    || combined
                        .call_ids
                        .get(digest)
                        .is_none_or(|combined| combined.0 & TERMINAL_CANDIDATES_MASK != 1))
        })
    }

    pub fn entry_count(&self) -> usize {
        self.call_ids.len()
    }
}

#[inline]
pub fn jsonl_terminal_call_id_digest(domain: &[u8], call_id: &str) -> [u8; 32] {
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
    fn duplicate_boundary_bits_preserve_order_and_same_region_saturation() {
        use JsonlTerminalObservationRegion::{AppendedSuffix, CertifiedPrefix};

        for (regions, expected) in [
            ([CertifiedPrefix, AppendedSuffix], true),
            ([AppendedSuffix, CertifiedPrefix], true),
            ([CertifiedPrefix, CertifiedPrefix], false),
            ([AppendedSuffix, AppendedSuffix], false),
        ] {
            let mut authority = JsonlTerminalAuthority::available();
            for region in regions {
                authority.observe_digest([0; 32], region, 8);
            }
            assert_eq!(authority.append_requires_replacement(), expected);
        }
    }

    #[test]
    fn unavailable_authority_preserves_legacy_fail_open_behavior() {
        assert!(JsonlTerminalAuthority::default().is_unique(DOMAIN, "unknown"));
    }

    #[test]
    fn checkpointed_digest_counts_preserve_positive_claims_and_invalidation() {
        let digest = jsonl_terminal_call_id_digest(DOMAIN, "call");
        let duplicate = jsonl_terminal_call_id_digest(DOMAIN, "duplicate");
        let prefix = JsonlCheckpointedTerminalAuthority::from_digest_counts(
            [(digest, 1), (duplicate, 2)],
            false,
        );
        let counts = prefix.digest_counts().collect::<BTreeMap<_, _>>();
        assert_eq!(counts.get(&digest), Some(&1));
        assert_eq!(counts.get(&duplicate), Some(&2));
        assert!(prefix.is_unique(DOMAIN, "call"));
        assert!(!prefix.is_unique(DOMAIN, "duplicate"));

        let mut combined = prefix.clone();
        combined.observe_digest(digest, JsonlTerminalObservationRegion::WholeSource, 8);
        assert!(prefix.positive_claim_invalidated_by(&combined));
    }
}
