use std::collections::HashMap;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlTerminalObservationRegion {
    WholeSource,
    CertifiedPrefix,
    AppendedSuffix,
}

#[derive(Debug, Clone, Copy, Default)]
struct JsonlTerminalMultiplicity {
    candidates: u8,
    in_certified_prefix: bool,
    in_appended_suffix: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlTerminalAuthority {
    call_ids: HashMap<[u8; 32], JsonlTerminalMultiplicity>,
    exhausted: bool,
    available: bool,
}

impl JsonlTerminalAuthority {
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
        self.available = true;
        if self.exhausted {
            return;
        }
        let digest = terminal_call_id_digest(domain, call_id);
        if !self.call_ids.contains_key(&digest) && self.call_ids.len() >= capacity {
            self.observe_ambiguous_terminal();
            return;
        }
        let state = self.call_ids.entry(digest).or_default();
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
        !self.available
            || (!self.exhausted
                && self
                    .call_ids
                    .get(&terminal_call_id_digest(domain, call_id))
                    .is_some_and(|state| state.candidates == 1))
    }

    pub(crate) fn append_requires_replacement(&self) -> bool {
        self.exhausted
            || self.call_ids.values().any(|state| {
                state.in_certified_prefix && state.in_appended_suffix && state.candidates > 1
            })
    }

    pub(crate) fn observe_ambiguous_terminal(&mut self) {
        self.available = true;
        self.call_ids.clear();
        self.exhausted = true;
    }
}

#[inline]
fn terminal_call_id_digest(domain: &[u8], call_id: &str) -> [u8; 32] {
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
}
