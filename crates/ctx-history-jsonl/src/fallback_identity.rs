use std::collections::HashMap;

use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    derive_event_id, EventIdentityInput, NativeItemKey, SourceKey, StableEntityId,
    SubrecordSelector, TypedKey,
};

use crate::{JsonlFamilyError, JsonlFamilyProjectionMode, JsonlResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackEventIdentityMode {
    Cold,
    CertifiedAppend,
    Replacement,
}

impl From<JsonlFamilyProjectionMode> for FallbackEventIdentityMode {
    fn from(mode: JsonlFamilyProjectionMode) -> Self {
        match mode {
            JsonlFamilyProjectionMode::Cold => Self::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend => Self::CertifiedAppend,
            JsonlFamilyProjectionMode::Replacement => Self::Replacement,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FallbackEventIdentityAssignment {
    native_item_key: NativeItemKey,
    native_event_id: TypedKey,
    #[cfg(test)]
    duplicate_occurrence: u64,
}

impl FallbackEventIdentityAssignment {
    pub fn native_item_key(&self) -> &NativeItemKey {
        &self.native_item_key
    }

    pub fn native_event_id(&self) -> &TypedKey {
        &self.native_event_id
    }

    #[cfg(test)]
    fn duplicate_occurrence(&self) -> u64 {
        self.duplicate_occurrence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FallbackGroupKey {
    fingerprint: TypedKey,
    subrecord_selector: Option<SubrecordSelector>,
}

struct FallbackGroupState {
    base_occurrences: u64,
    projected_occurrences: u64,
}

/// Assigns content-shaped fallback identities without making source position
/// part of the identity.
///
/// A certified append may safely continue an indistinguishable duplicate run,
/// because the family has proved that the complete old prefix is unchanged.
/// A replacement restarts occurrence numbering and reconciles every observed
/// duplicate group against the immutable Core base. If a group existed in the
/// current scheme and its cardinality changed, the replacement is ambiguous:
/// there is no provider evidence identifying which duplicate survived. The
/// caller must fail the source instead of adopting an arbitrary prior ID.
pub struct FallbackEventIdentityState<L: BaseEventLookup, E: JsonlFamilyError> {
    source: SourceKey,
    session_id: StableEntityId,
    logical_item_kind: String,
    native_item_namespace: String,
    identity_version: String,
    mode: FallbackEventIdentityMode,
    base_lookup: Option<L>,
    groups: HashMap<FallbackGroupKey, FallbackGroupState>,
    error: std::marker::PhantomData<fn() -> E>,
}

impl<L: BaseEventLookup, E: JsonlFamilyError> FallbackEventIdentityState<L, E> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: SourceKey,
        session_id: StableEntityId,
        logical_item_kind: impl Into<String>,
        native_item_namespace: impl Into<String>,
        identity_version: impl Into<String>,
        mode: FallbackEventIdentityMode,
        base_lookup: Option<L>,
    ) -> JsonlResult<Self, E> {
        match (mode, base_lookup.is_some()) {
            (FallbackEventIdentityMode::Cold, false)
            | (FallbackEventIdentityMode::CertifiedAppend, true)
            | (FallbackEventIdentityMode::Replacement, true) => {}
            _ => {
                return Err(E::system_invariant(
                    "fallback event identity mode has inconsistent Core base authority",
                ));
            }
        }
        Ok(Self {
            source,
            session_id,
            logical_item_kind: logical_item_kind.into(),
            native_item_namespace: native_item_namespace.into(),
            identity_version: identity_version.into(),
            mode,
            base_lookup,
            groups: HashMap::new(),
            error: std::marker::PhantomData,
        })
    }

    pub fn assign(
        &mut self,
        fingerprint: TypedKey,
        subrecord_selector: Option<&SubrecordSelector>,
    ) -> JsonlResult<FallbackEventIdentityAssignment, E> {
        fingerprint
            .validate_contract()
            .map_err(|error| E::invalid_payload(error.to_string()))?;
        let key = FallbackGroupKey {
            fingerprint,
            subrecord_selector: subrecord_selector.cloned(),
        };
        if !self.groups.contains_key(&key) {
            let base_occurrences = self.base_occurrence_count(&key)?;
            self.groups.insert(
                key.clone(),
                FallbackGroupState {
                    base_occurrences,
                    projected_occurrences: 0,
                },
            );
        }
        let group = self
            .groups
            .get_mut(&key)
            .ok_or_else(|| E::system_invariant("fallback event identity group disappeared"))?;
        let first_occurrence = match self.mode {
            FallbackEventIdentityMode::CertifiedAppend => group.base_occurrences,
            FallbackEventIdentityMode::Cold | FallbackEventIdentityMode::Replacement => 0,
        };
        let duplicate_occurrence = first_occurrence
            .checked_add(group.projected_occurrences)
            .ok_or_else(|| E::system_invariant("fallback event duplicate occurrence overflowed"))?;
        group.projected_occurrences = group
            .projected_occurrences
            .checked_add(1)
            .ok_or_else(|| E::system_invariant("fallback event duplicate occurrence overflowed"))?;
        self.assignment(&key, duplicate_occurrence)
    }

    pub fn finish(&self) -> JsonlResult<(), E> {
        if self.mode != FallbackEventIdentityMode::Replacement {
            return Ok(());
        }
        for group in self.groups.values() {
            if group.base_occurrences != 0 && group.base_occurrences != group.projected_occurrences
            {
                return Err(E::invalid_payload(format!(
                    "fallback event identity is ambiguous: an indistinguishable duplicate group changed from {} to {} records",
                    group.base_occurrences, group.projected_occurrences
                )));
            }
        }
        Ok(())
    }

    fn base_occurrence_count(&self, key: &FallbackGroupKey) -> JsonlResult<u64, E> {
        let Some(base_lookup) = self.base_lookup.as_ref() else {
            return Ok(0);
        };
        if !self.base_occurrence_exists(base_lookup, key, 0)? {
            return Ok(0);
        }
        let mut present = 0_u64;
        let mut missing = 1_u64;
        while self.base_occurrence_exists(base_lookup, key, missing)? {
            present = missing;
            missing = match missing.checked_mul(2) {
                Some(next) => next,
                None if missing != u64::MAX => u64::MAX,
                None => {
                    return Err(E::system_invariant(
                        "fallback event duplicate occurrence overflowed",
                    ));
                }
            };
        }
        while present.saturating_add(1) < missing {
            let candidate = present + (missing - present) / 2;
            if self.base_occurrence_exists(base_lookup, key, candidate)? {
                present = candidate;
            } else {
                missing = candidate;
            }
        }
        Ok(missing)
    }

    fn base_occurrence_exists(
        &self,
        base_lookup: &L,
        key: &FallbackGroupKey,
        occurrence: u64,
    ) -> JsonlResult<bool, E> {
        let assignment = self.assignment(key, occurrence)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: &self.logical_item_kind,
            native_item_key: assignment.native_item_key(),
            subrecord_selector: key.subrecord_selector.as_ref(),
        })
        .map_err(|error| E::invalid_payload(error.to_string()))?;
        base_lookup
            .contains(event_id.as_uuid())
            .map_err(|error| E::invalid_payload(error.to_string()))
    }

    fn assignment(
        &self,
        key: &FallbackGroupKey,
        duplicate_occurrence: u64,
    ) -> JsonlResult<FallbackEventIdentityAssignment, E> {
        let parts = vec![
            TypedKey::utf8(&self.identity_version)
                .map_err(|error| E::invalid_payload(error.to_string()))?,
            key.fingerprint.clone(),
            TypedKey::U64(duplicate_occurrence),
        ];
        let native_item_key = NativeItemKey::composite(&self.native_item_namespace, parts.clone())
            .map_err(|error| E::invalid_payload(error.to_string()))?;
        let native_event_id =
            TypedKey::composite(parts).map_err(|error| E::invalid_payload(error.to_string()))?;
        Ok(FallbackEventIdentityAssignment {
            native_item_key,
            native_event_id,
            #[cfg(test)]
            duplicate_occurrence,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, convert::Infallible, sync::Arc};

    use ctx_history_core::{
        derive_session_id, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    };
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::*;

    const LOGICAL_ITEM_KIND: &str = "fallback-test-event";
    const NATIVE_ITEM_NAMESPACE: &str = "fallback.test.event";
    const IDENTITY_VERSION: &str = "fallback-test-v1";

    #[derive(Clone, Default)]
    struct SetLookup(Arc<HashSet<Uuid>>);

    impl BaseEventLookup for SetLookup {
        type Error = Infallible;

        fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
            Ok(self.0.contains(&event_id))
        }
    }

    fn source_and_session() -> (SourceKey, StableEntityId) {
        let source = SourceKey::derive(
            "pi",
            "pi_session_jsonl",
            "fallback-test-v1",
            1,
            SourceAnchor::provider_native(
                "fallback.test.session",
                TypedKey::utf8("session").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let native_session_key = NativeSessionKey::native_id(
            "fallback.test.session",
            TypedKey::utf8("session").unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "fallback-test-session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        (source, session_id)
    }

    fn fingerprint(value: &str) -> TypedKey {
        let mut digest = Sha256::new();
        digest.update(b"fallback-test-fingerprint-v1\0");
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
        TypedKey::bytes(digest.finalize().to_vec()).unwrap()
    }

    fn assignments(
        source: &SourceKey,
        session_id: StableEntityId,
        mode: FallbackEventIdentityMode,
        lookup: Option<SetLookup>,
        values: &[&str],
    ) -> (
        Vec<(StableEntityId, TypedKey)>,
        JsonlResult<(), ctx_history_source_io::SourceIoError>,
    ) {
        let mut state = FallbackEventIdentityState::<_, ctx_history_source_io::SourceIoError>::new(
            source.clone(),
            session_id,
            LOGICAL_ITEM_KIND,
            NATIVE_ITEM_NAMESPACE,
            IDENTITY_VERSION,
            mode,
            lookup,
        )
        .unwrap();
        let events = values
            .iter()
            .map(|value| {
                let assignment = state.assign(fingerprint(value), None).unwrap();
                let event_id = derive_event_id(EventIdentityInput {
                    source,
                    session_id,
                    logical_item_kind: LOGICAL_ITEM_KIND,
                    native_item_key: assignment.native_item_key(),
                    subrecord_selector: None,
                })
                .unwrap();
                (event_id, assignment.native_event_id().clone())
            })
            .collect();
        (events, state.finish())
    }

    fn lookup(events: &[(StableEntityId, TypedKey)]) -> SetLookup {
        SetLookup(Arc::new(
            events
                .iter()
                .map(|(event_id, _)| event_id.as_uuid())
                .collect(),
        ))
    }

    #[test]
    fn fallback_assignment_preserves_replacement_identity_by_content() {
        let (source, session_id) = source_and_session();
        let (baseline, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Cold,
            None,
            &["anchor", "target", "suffix"],
        );
        finished.unwrap();
        let (current, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Replacement,
            Some(lookup(&baseline)),
            &["inserted", "anchor", "target", "suffix"],
        );
        finished.unwrap();
        assert_eq!(baseline[0].0, current[1].0);
        assert_eq!(baseline[1].0, current[2].0);
        assert_eq!(baseline[2].0, current[3].0);
    }

    #[test]
    fn duplicate_groups_continue_append_and_reject_ambiguous_replacement() {
        let (source, session_id) = source_and_session();
        let (baseline, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Cold,
            None,
            &["anchor", "duplicate", "duplicate", "suffix"],
        );
        finished.unwrap();
        let lookup = lookup(&baseline);

        let (_, ambiguous) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Replacement,
            Some(lookup.clone()),
            &["anchor", "duplicate", "suffix"],
        );
        assert!(ambiguous
            .unwrap_err()
            .to_string()
            .contains("changed from 2 to 1"));

        let mut append =
            FallbackEventIdentityState::<_, ctx_history_source_io::SourceIoError>::new(
                source,
                session_id,
                LOGICAL_ITEM_KIND,
                NATIVE_ITEM_NAMESPACE,
                IDENTITY_VERSION,
                FallbackEventIdentityMode::CertifiedAppend,
                Some(lookup),
            )
            .unwrap();
        assert_eq!(
            append
                .assign(fingerprint("duplicate"), None)
                .unwrap()
                .duplicate_occurrence(),
            2
        );
        append.finish().unwrap();
    }
}
