use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};

use ctx_history_core::ProviderNativeSessionRelationship;

pub(crate) const MAX_CODEX_SESSION_META_PREFIX_RECORDS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSessionMetaIdentity {
    pub(crate) native_session_id: String,
    pub(crate) parent_native_session_id: Option<String>,
    pub(crate) root_native_session_id: Option<String>,
    pub(crate) session_relationship: Option<ProviderNativeSessionRelationship>,
}

/// Selects the one file owner while admitting only provider-declared ancestor
/// metadata reachable from that owner. The records may appear owner-first or
/// ancestor-first, but disconnected owners, branches, cycles, and inconsistent
/// repeats remain invalid.
pub(crate) fn select_codex_session_meta_owner(
    metadata: &[CodexSessionMetaIdentity],
    expected_owner: Option<&str>,
) -> Option<usize> {
    if metadata.is_empty() {
        return None;
    }

    let mut unique = BTreeMap::<&str, usize>::new();
    for (index, candidate) in metadata.iter().enumerate() {
        if !valid_relationship(candidate) {
            return None;
        }
        match unique.entry(candidate.native_session_id.as_str()) {
            Entry::Vacant(entry) => {
                entry.insert(index);
            }
            Entry::Occupied(entry) if metadata[*entry.get()] == *candidate => {}
            Entry::Occupied(_) => return None,
        }
    }

    let owner_index = match expected_owner {
        Some(expected_owner) => *unique.get(expected_owner)?,
        None => {
            let referenced = unique
                .values()
                .filter_map(|index| metadata[*index].parent_native_session_id.as_deref())
                .filter(|parent| unique.contains_key(parent))
                .collect::<BTreeSet<_>>();
            let mut candidates = unique
                .iter()
                .filter(|(native_session_id, _)| !referenced.contains(*native_session_id))
                .map(|(_, index)| *index);
            let owner = candidates.next()?;
            if candidates.next().is_some() {
                return None;
            }
            owner
        }
    };

    let mut visited = BTreeSet::new();
    let mut current = owner_index;
    loop {
        let identity = &metadata[current];
        if !visited.insert(identity.native_session_id.as_str()) {
            return None;
        }
        let Some(parent) = identity.parent_native_session_id.as_deref() else {
            break;
        };
        let Some(parent_index) = unique.get(parent) else {
            break;
        };
        current = *parent_index;
    }
    (visited.len() == unique.len()).then_some(owner_index)
}

fn valid_relationship(identity: &CodexSessionMetaIdentity) -> bool {
    match (
        identity.session_relationship,
        identity.parent_native_session_id.as_ref(),
    ) {
        (Some(ProviderNativeSessionRelationship::Root), None) | (None, None) => true,
        (Some(ProviderNativeSessionRelationship::Root), Some(_)) => false,
        (Some(_), Some(parent)) => parent != &identity.native_session_id,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(
        native_session_id: &str,
        parent_native_session_id: Option<&str>,
    ) -> CodexSessionMetaIdentity {
        CodexSessionMetaIdentity {
            native_session_id: native_session_id.to_owned(),
            parent_native_session_id: parent_native_session_id.map(str::to_owned),
            root_native_session_id: None,
            session_relationship: Some(if parent_native_session_id.is_some() {
                ProviderNativeSessionRelationship::Forked
            } else {
                ProviderNativeSessionRelationship::Root
            }),
        }
    }

    #[test]
    fn selects_one_declared_owner_in_either_metadata_order() {
        let owner = identity("owner", Some("parent"));
        let parent = identity("parent", Some("root"));
        let root = identity("root", None);
        for metadata in [
            vec![owner.clone(), parent.clone(), root.clone()],
            vec![root.clone(), parent.clone(), owner.clone()],
        ] {
            let selected = select_codex_session_meta_owner(&metadata, Some("owner")).unwrap();
            assert_eq!(metadata[selected].native_session_id, "owner");
        }
    }

    #[test]
    fn rejects_disconnected_branch_cycle_and_conflicting_repeat() {
        for metadata in [
            vec![identity("owner", None), identity("unrelated", None)],
            vec![
                identity("owner", Some("parent")),
                identity("other", Some("parent")),
            ],
            vec![
                identity("owner", Some("parent")),
                identity("parent", Some("owner")),
            ],
            vec![identity("owner", None), identity("owner", Some("parent"))],
        ] {
            assert!(select_codex_session_meta_owner(&metadata, Some("owner")).is_none());
        }
    }
}
