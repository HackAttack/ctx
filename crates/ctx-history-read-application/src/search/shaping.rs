use std::collections::{BTreeMap, VecDeque};

use ctx_history_core::AgentScope;
use ctx_history_index_query::EventSearchCandidate;
use uuid::Uuid;

use super::{search_candidate_order, SearchEventMetadata, SearchHit, SearchResultWindow};

pub(super) const PRIMARY_CHAMPION_SCORE_TOLERANCE: f32 = 0.95;

pub(super) fn root_first_candidate_pool_is_decisive(
    candidates: &[EventSearchCandidate],
    limit: usize,
    source_tail_score: f32,
) -> bool {
    if limit == 0 {
        return true;
    }
    let mut roots = BTreeMap::<Uuid, (f32, Option<f32>)>::new();
    for candidate in candidates {
        let session_id = candidate.event.session_id.as_uuid();
        let root_id = candidate
            .event
            .root_session_id
            .map(|id| id.as_uuid())
            .unwrap_or(session_id);
        roots
            .entry(root_id)
            .and_modify(|(strongest_score, strongest_primary_score)| {
                if candidate.score.total_cmp(strongest_score).is_gt() {
                    *strongest_score = candidate.score;
                }
                if candidate.event.agent_scope == Some(AgentScope::Primary)
                    && strongest_primary_score
                        .is_none_or(|score| candidate.score.total_cmp(&score).is_gt())
                {
                    *strongest_primary_score = Some(candidate.score);
                }
            })
            .or_insert((
                candidate.score,
                (candidate.event.agent_scope == Some(AgentScope::Primary))
                    .then_some(candidate.score),
            ));
    }

    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort_by(|(left_id, (left_score, _)), (right_id, (right_score, _))| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_id.cmp(right_id))
    });
    let selected = roots.iter().take(limit).collect::<Vec<_>>();
    if selected.len() < limit {
        return false;
    }

    let weakest_selected_score = selected
        .last()
        .map(|(_, (score, _))| *score)
        .unwrap_or(f32::INFINITY);
    if source_tail_score >= weakest_selected_score {
        return false;
    }

    selected
        .iter()
        .all(|(_, (strongest_score, strongest_primary_score))| {
            let primary_threshold = *strongest_score * PRIMARY_CHAMPION_SCORE_TOLERANCE;
            strongest_primary_score
                .filter(|score| *score >= primary_threshold)
                .map_or(source_tail_score < primary_threshold, |score| {
                    source_tail_score < score
                })
        })
}

pub fn shape_search_result_window<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    limit: usize,
    event_results: bool,
) -> SearchResultWindow {
    let shape_limit = limit.saturating_add(1);
    let mut hits = if event_results {
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort_by(|left, right| search_candidate_order(left, right));
        candidates
            .into_iter()
            .take(shape_limit)
            .map(|candidate| SearchHit {
                event: SearchEventMetadata::from(&candidate.event),
                score: candidate.score,
                more_matches_in_session: 0,
            })
            .collect()
    } else {
        shape_root_first_session_hits(candidates, shape_limit)
    };
    let more_available = hits.len() > limit;
    hits.truncate(limit);
    SearchResultWindow {
        limit,
        hits,
        more_available,
    }
}

struct SessionChampion<'a> {
    candidate: &'a EventSearchCandidate,
    match_count: usize,
}

struct RootSessionHits {
    root_id: Uuid,
    strongest_score: f32,
    champion: SearchHit,
    remaining: VecDeque<SearchHit>,
}

fn shape_root_first_session_hits<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    shape_limit: usize,
) -> Vec<SearchHit> {
    let mut sessions = BTreeMap::<Uuid, SessionChampion<'a>>::new();
    for candidate in candidates {
        let session_id = candidate.event.session_id.as_uuid();
        sessions
            .entry(session_id)
            .and_modify(|session| {
                session.match_count = session.match_count.saturating_add(1);
                if search_candidate_order(candidate, session.candidate).is_lt() {
                    session.candidate = candidate;
                }
            })
            .or_insert(SessionChampion {
                candidate,
                match_count: 1,
            });
    }

    let mut sessions_by_root = BTreeMap::<Uuid, Vec<SessionChampion<'a>>>::new();
    for session in sessions.into_values() {
        let session_id = session.candidate.event.session_id.as_uuid();
        let root_id = session
            .candidate
            .event
            .root_session_id
            .map(|id| id.as_uuid())
            .unwrap_or(session_id);
        sessions_by_root.entry(root_id).or_default().push(session);
    }

    let mut roots = Vec::<RootSessionHits>::with_capacity(sessions_by_root.len());
    for (root_id, mut sessions) in sessions_by_root {
        sessions.sort_by(|left, right| {
            search_candidate_order(left.candidate, right.candidate).then_with(|| {
                left.candidate
                    .event
                    .session_id
                    .as_uuid()
                    .cmp(&right.candidate.event.session_id.as_uuid())
            })
        });
        let Some(strongest) = sessions.first() else {
            continue;
        };
        let strongest_score = strongest.candidate.score;
        let preferred_primary = sessions.iter().position(|session| {
            session.candidate.event.agent_scope == Some(AgentScope::Primary)
                && session.candidate.score >= strongest_score * PRIMARY_CHAMPION_SCORE_TOLERANCE
        });
        let champion_position = preferred_primary.unwrap_or(0);
        let mut hits = sessions
            .into_iter()
            .map(session_champion_hit)
            .collect::<Vec<_>>();
        if champion_position >= hits.len() {
            continue;
        }
        let champion = hits.remove(champion_position);
        roots.push(RootSessionHits {
            root_id,
            strongest_score,
            champion,
            remaining: hits.into(),
        });
    }

    roots.sort_by(|left, right| {
        right
            .strongest_score
            .total_cmp(&left.strongest_score)
            .then_with(|| left.root_id.cmp(&right.root_id))
    });

    let mut hits = roots
        .iter()
        .take(shape_limit)
        .map(|root| root.champion.clone())
        .collect::<Vec<_>>();
    while hits.len() < shape_limit {
        let mut added = false;
        for root in &mut roots {
            let Some(hit) = root.remaining.pop_front() else {
                continue;
            };
            hits.push(hit);
            added = true;
            if hits.len() == shape_limit {
                break;
            }
        }
        if !added {
            break;
        }
    }
    hits
}

fn session_champion_hit(session: SessionChampion<'_>) -> SearchHit {
    SearchHit {
        event: SearchEventMetadata::from(&session.candidate.event),
        score: session.candidate.score,
        more_matches_in_session: session.match_count.saturating_sub(1),
    }
}
