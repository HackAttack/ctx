use std::collections::BTreeSet;

use anyhow::Result;
use ctx_history_index_query::{ExcludedSessionTree, SessionRecord, VerifiedIndex};
use uuid::Uuid;

use super::ActiveSessionExclusion;

pub(super) const MAX_ACTIVE_SESSION_ANCESTORS: usize = 64;

pub(super) fn excluded_active_session_tree(
    index: &VerifiedIndex,
    active_session: &ActiveSessionExclusion,
) -> Result<ExcludedSessionTree> {
    let sessions = index.sessions_by_provider_session_id(
        &active_session.provider_session_id,
        Some(&active_session.provider),
    )?;
    let ancestries = sessions
        .iter()
        .map(SessionAncestry::from)
        .collect::<Vec<_>>();
    let session_id = resolved_unique_session_tree_root_id(&ancestries, |session_id| {
        Ok(index
            .session_by_id(session_id)?
            .as_ref()
            .map(SessionAncestry::from))
    })?;
    Ok(ExcludedSessionTree {
        provider: active_session.provider.clone(),
        provider_session_id: active_session.provider_session_id.clone(),
        session_id,
    })
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionAncestry {
    pub(super) session_id: Uuid,
    pub(super) parent_session_id: Option<Uuid>,
    pub(super) claimed_root_session_id: Option<Uuid>,
}

impl From<&SessionRecord> for SessionAncestry {
    fn from(session: &SessionRecord) -> Self {
        Self {
            session_id: session.session_id.as_uuid(),
            parent_session_id: session.parent_session_id.map(|id| id.as_uuid()),
            claimed_root_session_id: session.root_session_id.map(|id| id.as_uuid()),
        }
    }
}

pub(super) fn resolved_unique_session_tree_root_id<F>(
    sessions: &[SessionAncestry],
    session_by_id: F,
) -> Result<Option<Uuid>>
where
    F: FnMut(Uuid) -> Result<Option<SessionAncestry>>,
{
    let [session] = sessions else {
        return Ok(None);
    };
    resolved_session_tree_root_id(*session, session_by_id)
}

fn resolved_session_tree_root_id<F>(
    session: SessionAncestry,
    mut session_by_id: F,
) -> Result<Option<Uuid>>
where
    F: FnMut(Uuid) -> Result<Option<SessionAncestry>>,
{
    // Prove the complete parent chain against the pinned generation. Codex
    // may put an immediate parent in root_session_id for deeper descendants,
    // so a stored root is accepted only when it names a proven ancestor.
    let mut current = session;
    let mut visited = BTreeSet::new();
    let mut ancestry = Vec::with_capacity(MAX_ACTIVE_SESSION_ANCESTORS + 1);
    let root_id = loop {
        if !visited.insert(current.session_id) {
            return Ok(None);
        }
        ancestry.push(current);
        let Some(parent_id) = current.parent_session_id else {
            break current.session_id;
        };
        if ancestry.len() > MAX_ACTIVE_SESSION_ANCESTORS {
            return Ok(None);
        }
        let Some(parent) = session_by_id(parent_id)? else {
            return Ok(None);
        };
        current = parent;
    };

    for (position, session) in ancestry.iter().enumerate() {
        let Some(claimed_root_id) = session.claimed_root_session_id else {
            continue;
        };
        let claim_is_proven = if position + 1 == ancestry.len() {
            claimed_root_id == session.session_id
        } else {
            ancestry[position + 1..]
                .iter()
                .any(|ancestor| ancestor.session_id == claimed_root_id)
        };
        if !claim_is_proven {
            return Ok(None);
        }
    }

    Ok(Some(root_id))
}
