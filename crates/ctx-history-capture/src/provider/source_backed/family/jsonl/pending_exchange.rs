use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JsonlPendingExchangeState<T> {
    Exact(T),
    Ambiguous,
}

#[derive(Debug)]
pub(crate) enum JsonlPendingExchangeLookup<T> {
    Exact(T),
    Ambiguous,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlPendingExchangeRemember {
    Inserted,
    BecameAmbiguous,
    CapacityExceeded,
}

pub(crate) trait JsonlPendingExchangeStorage<T> {
    fn len(&self) -> usize;
    fn get_mut(&mut self, identity: &str) -> Option<&mut JsonlPendingExchangeState<T>>;
    fn insert(&mut self, identity: String, state: JsonlPendingExchangeState<T>);
    fn remove(&mut self, identity: &str) -> Option<JsonlPendingExchangeState<T>>;
}

impl<T> JsonlPendingExchangeStorage<T> for HashMap<String, JsonlPendingExchangeState<T>> {
    fn len(&self) -> usize {
        HashMap::len(self)
    }

    fn get_mut(&mut self, identity: &str) -> Option<&mut JsonlPendingExchangeState<T>> {
        HashMap::get_mut(self, identity)
    }

    fn insert(&mut self, identity: String, state: JsonlPendingExchangeState<T>) {
        HashMap::insert(self, identity, state);
    }

    fn remove(&mut self, identity: &str) -> Option<JsonlPendingExchangeState<T>> {
        HashMap::remove(self, identity)
    }
}

impl<T> JsonlPendingExchangeStorage<T> for BTreeMap<String, JsonlPendingExchangeState<T>> {
    fn len(&self) -> usize {
        BTreeMap::len(self)
    }

    fn get_mut(&mut self, identity: &str) -> Option<&mut JsonlPendingExchangeState<T>> {
        BTreeMap::get_mut(self, identity)
    }

    fn insert(&mut self, identity: String, state: JsonlPendingExchangeState<T>) {
        BTreeMap::insert(self, identity, state);
    }

    fn remove(&mut self, identity: &str) -> Option<JsonlPendingExchangeState<T>> {
        BTreeMap::remove(self, identity)
    }
}

pub(crate) fn remember_pending_exchange<T>(
    states: &mut impl JsonlPendingExchangeStorage<T>,
    identity: &str,
    state: JsonlPendingExchangeState<T>,
    capacity: usize,
) -> JsonlPendingExchangeRemember {
    if let Some(existing) = states.get_mut(identity) {
        *existing = JsonlPendingExchangeState::Ambiguous;
        return JsonlPendingExchangeRemember::BecameAmbiguous;
    }
    if states.len() >= capacity {
        return JsonlPendingExchangeRemember::CapacityExceeded;
    }
    states.insert(identity.to_owned(), state);
    JsonlPendingExchangeRemember::Inserted
}

pub(crate) fn take_pending_exchange<T>(
    states: &mut impl JsonlPendingExchangeStorage<T>,
    identity: Option<&str>,
) -> JsonlPendingExchangeLookup<T> {
    match identity.and_then(|identity| states.remove(identity)) {
        Some(JsonlPendingExchangeState::Exact(context)) => {
            JsonlPendingExchangeLookup::Exact(context)
        }
        Some(JsonlPendingExchangeState::Ambiguous) => JsonlPendingExchangeLookup::Ambiguous,
        None => JsonlPendingExchangeLookup::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_identity_becomes_ambiguous_and_is_consumed_once() {
        let mut states = HashMap::new();
        assert_eq!(
            remember_pending_exchange(
                &mut states,
                "call",
                JsonlPendingExchangeState::Exact(1_u8),
                2,
            ),
            JsonlPendingExchangeRemember::Inserted
        );
        assert_eq!(
            remember_pending_exchange(
                &mut states,
                "call",
                JsonlPendingExchangeState::Exact(2_u8),
                2,
            ),
            JsonlPendingExchangeRemember::BecameAmbiguous
        );
        assert!(matches!(
            take_pending_exchange(&mut states, Some("call")),
            JsonlPendingExchangeLookup::Ambiguous
        ));
        assert!(matches!(
            take_pending_exchange(&mut states, Some("call")),
            JsonlPendingExchangeLookup::Missing
        ));
    }

    #[test]
    fn ordered_and_hash_storage_share_capacity_behavior() {
        fn exercise(states: &mut impl JsonlPendingExchangeStorage<u8>) {
            assert_eq!(
                remember_pending_exchange(states, "one", JsonlPendingExchangeState::Exact(1), 1,),
                JsonlPendingExchangeRemember::Inserted
            );
            assert_eq!(
                remember_pending_exchange(states, "two", JsonlPendingExchangeState::Exact(2), 1,),
                JsonlPendingExchangeRemember::CapacityExceeded
            );
        }

        exercise(&mut HashMap::new());
        exercise(&mut BTreeMap::new());
    }
}
