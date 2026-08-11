use std::{
    collections::{BTreeMap, HashMap},
    marker::PhantomData,
};

use ctx_history_index::BaseEventIdentityLookup;

/// Assigns contiguous duplicate occurrences for content-shaped JSONL event
/// identities. Certified appends resume after the immutable Core prefix;
/// cold and replacement projections restart from zero.
pub struct JsonlAppendOccurrenceState<K, S = HashMap<K, u64>> {
    base_lookup: Option<BaseEventIdentityLookup>,
    next_occurrences: S,
    _key: PhantomData<K>,
}

pub type JsonlOrderedAppendOccurrenceState<K> = JsonlAppendOccurrenceState<K, BTreeMap<K, u64>>;

pub trait JsonlOccurrenceStorage<K> {
    fn get(&self, key: &K) -> Option<u64>;
    fn insert(&mut self, key: K, next_occurrence: u64);
}

impl<K: Eq + std::hash::Hash> JsonlOccurrenceStorage<K> for HashMap<K, u64> {
    fn get(&self, key: &K) -> Option<u64> {
        HashMap::get(self, key).copied()
    }

    fn insert(&mut self, key: K, next_occurrence: u64) {
        let _ = HashMap::insert(self, key, next_occurrence);
    }
}

impl<K: Ord> JsonlOccurrenceStorage<K> for BTreeMap<K, u64> {
    fn get(&self, key: &K) -> Option<u64> {
        BTreeMap::get(self, key).copied()
    }

    fn insert(&mut self, key: K, next_occurrence: u64) {
        let _ = BTreeMap::insert(self, key, next_occurrence);
    }
}

impl<K, S: Default> Default for JsonlAppendOccurrenceState<K, S> {
    fn default() -> Self {
        Self {
            base_lookup: None,
            next_occurrences: S::default(),
            _key: PhantomData,
        }
    }
}

impl<K, S: Default + JsonlOccurrenceStorage<K>> JsonlAppendOccurrenceState<K, S> {
    pub fn for_append(base_lookup: BaseEventIdentityLookup) -> Self {
        Self {
            base_lookup: Some(base_lookup),
            next_occurrences: S::default(),
            _key: PhantomData,
        }
    }

    #[inline]
    pub fn next<E>(
        &mut self,
        key: K,
        mut overflow: impl FnMut() -> E,
        mut base_occurrence_exists: impl FnMut(&BaseEventIdentityLookup, u64) -> Result<bool, E>,
    ) -> Result<u64, E> {
        let occurrence = match self.next_occurrences.get(&key) {
            Some(occurrence) => occurrence,
            None => first_unused_base_occurrence(
                self.base_lookup.as_ref(),
                &mut overflow,
                &mut base_occurrence_exists,
            )?,
        };
        self.next_occurrences
            .insert(key, occurrence.checked_add(1).ok_or_else(&mut overflow)?);
        Ok(occurrence)
    }

    #[doc(hidden)]
    pub fn set_next_occurrence_for_test(&mut self, key: K, next_occurrence: u64) {
        self.next_occurrences.insert(key, next_occurrence);
    }
}

fn first_unused_base_occurrence<E>(
    base_lookup: Option<&BaseEventIdentityLookup>,
    overflow: &mut impl FnMut() -> E,
    base_occurrence_exists: &mut impl FnMut(&BaseEventIdentityLookup, u64) -> Result<bool, E>,
) -> Result<u64, E> {
    let Some(base_lookup) = base_lookup else {
        return Ok(0);
    };
    if !base_occurrence_exists(base_lookup, 0)? {
        return Ok(0);
    }

    let mut present = 0_u64;
    let mut missing = 1_u64;
    while base_occurrence_exists(base_lookup, missing)? {
        present = missing;
        missing = match missing.checked_mul(2) {
            Some(next) => next,
            None if missing != u64::MAX => u64::MAX,
            None => return Err(overflow()),
        };
    }
    while present.saturating_add(1) < missing {
        let candidate = present + (missing - present) / 2;
        if base_occurrence_exists(base_lookup, candidate)? {
            present = candidate;
        } else {
            missing = candidate;
        }
    }
    Ok(missing)
}
