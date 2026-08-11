use ctx_history_index_query::{
    CopiedEventLineage, EventSearchFilters, VerifiedIndex, SEARCH_COPIED_EVENT_LINEAGE_POLICY,
};

use crate::presentation::presentations_for_search_hits;
use crate::{
    collect_search_hits, normalize_search_request, resolve_search_backend,
    search_filters_with_refs, validate_search_request, ActiveSessionExclusion, CompactRefResolver,
    HistorySemanticPort, NormalizedSearchQuery, SearchCollection, SearchExecutionResult,
    SearchPolicy, SearchPresentation, SearchRequest,
};

/// Query implementation contract for one caller-supplied, already-verified
/// Core generation. The optional peer is likewise supplied by the caller and
/// is used only to resolve compact selectors retained from the prior generation.
pub struct PinnedHistoryQuery<'index> {
    pub(crate) index: &'index VerifiedIndex,
    pub(crate) references: CompactRefResolver<'index>,
}

impl<'index> PinnedHistoryQuery<'index> {
    pub const fn new(
        index: &'index VerifiedIndex,
        retained_peer: Option<&'index VerifiedIndex>,
    ) -> Self {
        Self {
            index,
            references: CompactRefResolver::new(index, retained_peer),
        }
    }

    pub const fn index(&self) -> &'index VerifiedIndex {
        self.index
    }

    pub fn search<P: HistorySemanticPort>(
        &self,
        plan: PlannedSearch,
        active_session: Option<&ActiveSessionExclusion>,
        semantic_port: &P,
    ) -> SearchExecutionResult<SearchQueryResult> {
        let PlannedSearch { request, policy } = plan;
        let filters =
            search_filters_with_refs(&request, self.index, &self.references, active_session)?;
        let collection = collect_search_hits(
            &request,
            self.index,
            &filters,
            policy.semantic,
            semantic_port,
        )?;
        let presentations = presentations_for_search_hits(
            self.index,
            &collection.result_window.hits,
            &NormalizedSearchQuery::from_request(&request),
        )?;
        let copied_lineages = collection
            .result_window
            .hits
            .iter()
            .map(|hit| {
                self.index
                    .copied_event_lineage(hit.event.event_id, SEARCH_COPIED_EVENT_LINEAGE_POLICY)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(SearchQueryResult {
            request,
            filters,
            collection,
            presentations,
            copied_lineages,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PlannedSearch {
    request: SearchRequest,
    policy: SearchPolicy,
}

impl PlannedSearch {
    pub fn request(&self) -> &SearchRequest {
        &self.request
    }

    pub const fn policy(&self) -> SearchPolicy {
        self.policy
    }
}

pub fn plan_search(
    mut request: SearchRequest,
    policy: SearchPolicy,
) -> SearchExecutionResult<PlannedSearch> {
    normalize_search_request(&mut request)?;
    validate_search_request(&request)?;
    request.backend = Some(resolve_search_backend(&request, policy)?);
    Ok(PlannedSearch { request, policy })
}

#[derive(Debug)]
pub struct SearchQueryResult {
    pub request: SearchRequest,
    pub filters: EventSearchFilters,
    pub collection: SearchCollection,
    pub presentations: Vec<SearchPresentation>,
    pub copied_lineages: Vec<CopiedEventLineage>,
}
