//! Runtime-neutral contracts used by capture implementations.
//!
//! This crate intentionally owns no provider, source, JSONL, or index
//! implementation. Capture-side adapters select a concrete lookup type at
//! compile time, so this boundary adds neither dynamic dispatch nor storage.

use std::error::Error;

use uuid::Uuid;

/// Looks up exact event identities from an immutable capture base.
pub trait BaseEventLookup: Clone + Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error>;
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeLookup {
        event_ids: HashSet<Uuid>,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("fake lookup failed")]
    struct FakeLookupError;

    impl BaseEventLookup for FakeLookup {
        type Error = FakeLookupError;

        fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
            Ok(self.event_ids.contains(&event_id))
        }
    }

    fn lookup_contains<L: BaseEventLookup>(lookup: &L, event_id: Uuid) -> Result<bool, L::Error> {
        lookup.contains(event_id)
    }

    #[test]
    fn fake_lookup_is_static_generic_and_exact() {
        let present = Uuid::new_v4();
        let absent = Uuid::new_v4();
        let lookup = FakeLookup {
            event_ids: HashSet::from([present]),
        };

        assert!(lookup_contains(&lookup, present).unwrap());
        assert!(!lookup_contains(&lookup, absent).unwrap());
    }
}
