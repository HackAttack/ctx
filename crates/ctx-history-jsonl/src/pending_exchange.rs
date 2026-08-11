use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonlPendingExchangeState<T> {
    Exact(T),
    Ambiguous,
}

#[derive(Debug)]
pub enum JsonlPendingExchangeLookup<T> {
    Exact(T),
    Ambiguous,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlPendingExchangeRemember {
    Inserted,
    BecameAmbiguous,
    CapacityExceeded,
}

pub trait JsonlPendingExchangeStorage<T> {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
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

#[inline]
pub fn remember_pending_exchange<T>(
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

#[inline]
pub fn take_pending_exchange<T>(
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

pub fn sorted_pending_exchange_entries<T: Clone>(
    states: &HashMap<String, JsonlPendingExchangeState<T>>,
) -> Vec<(String, JsonlPendingExchangeState<T>)> {
    let mut entries = states
        .iter()
        .map(|(identity, state)| (identity.clone(), state.clone()))
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    entries
}

pub fn ordered_pending_exchange_entries<T: Clone>(
    states: &BTreeMap<String, JsonlPendingExchangeState<T>>,
) -> Vec<(String, JsonlPendingExchangeState<T>)> {
    states
        .iter()
        .map(|(identity, state)| (identity.clone(), state.clone()))
        .collect()
}

pub fn restore_hash_pending_exchange_entries<T>(
    entries: Vec<(String, JsonlPendingExchangeState<T>)>,
) -> Option<HashMap<String, JsonlPendingExchangeState<T>>> {
    let mut restored = HashMap::with_capacity(entries.len());
    for (identity, state) in entries {
        if identity.is_empty() || restored.insert(identity, state).is_some() {
            return None;
        }
    }
    Some(restored)
}

pub fn restore_ordered_pending_exchange_entries<T>(
    entries: Vec<(String, JsonlPendingExchangeState<T>)>,
) -> Option<BTreeMap<String, JsonlPendingExchangeState<T>>> {
    let mut restored = BTreeMap::new();
    for (identity, state) in entries {
        if identity.is_empty() || restored.insert(identity, state).is_some() {
            return None;
        }
    }
    Some(restored)
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
