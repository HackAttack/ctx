use std::time::{Duration, Instant};

use anyhow::Result;
use ctx_history_index_query::{
    CopiedEventLineage, EventSearchFilters, VerifiedIndex, SEARCH_COPIED_EVENT_LINEAGE_POLICY,
};
use serde_json::Value;

use crate::generation::PinnedGenerationRead;
use crate::presentation::presentations_for_search_hits;
use crate::{
    collect_search_hits, copied_lineage_read_model, normalize_search_request,
    reference_needs_retained_peer, render_search_json, resolve_search_backend,
    search_filters_with_refs, validate_search_request, ActiveSessionExclusion,
    CompactPresentationProjection, CompactRefResolver, GenerationReadError, GenerationReadPort,
    GenerationReadReceipt, GenerationReadRequest, GenerationReadTarget, HistorySemanticPort,
    NormalizedSearchQuery, RetainedPeerRead, SearchCollection, SearchExecutionError,
    SearchExecutionResult, SearchJsonInput, SearchPolicy, SearchPresentation, SearchRenderMetrics,
    SearchRequest, SearchResultCommands,
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

    pub(crate) fn search<P: HistorySemanticPort>(
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

pub struct SearchApplicationRequest {
    pub plan: PlannedSearch,
    pub generation_target: GenerationReadTarget,
    pub compact_projection: bool,
    pub active_session: Option<ActiveSessionExclusion>,
}

impl SearchApplicationRequest {
    fn retained_peer_read(&self) -> RetainedPeerRead {
        let compact_selector = self
            .plan
            .request()
            .session
            .as_deref()
            .is_some_and(reference_needs_retained_peer);
        if self.compact_projection || compact_selector {
            RetainedPeerRead::IfAvailable
        } else {
            RetainedPeerRead::Omit
        }
    }
}

#[derive(Debug)]
pub enum SearchApplicationError<GenerationError> {
    Generation(GenerationReadError<GenerationError>),
    Query(SearchExecutionError),
}

pub struct SearchApplicationResult {
    generation: PinnedGenerationRead,
    query: SearchQueryResult,
    copied_lineage_read_models: Vec<Value>,
    query_duration: Duration,
}

impl SearchApplicationResult {
    pub const fn index(&self) -> &VerifiedIndex {
        self.generation.index()
    }

    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub const fn query(&self) -> &SearchQueryResult {
        &self.query
    }

    pub fn copied_lineage_read_models(&self) -> &[Value] {
        &self.copied_lineage_read_models
    }

    pub const fn query_duration(&self) -> Duration {
        self.query_duration
    }

    pub fn render_read_model(&self, input: SearchApplicationReadModelInput<'_>) -> Result<Value> {
        render_search_json(SearchJsonInput {
            request: &self.query.request,
            index: self.generation.index(),
            collection: &self.query.collection,
            filters: &self.query.filters,
            presentations: &self.query.presentations,
            copied_lineages: &self.copied_lineage_read_models,
            commands: input.commands,
            freshness_mode: input.freshness_mode,
            generated_at: input.generated_at,
            semantic_fallback_code: input.semantic_fallback_code,
            semantic_fallback_detail: input.semantic_fallback_detail,
            metrics: input.metrics,
        })
    }

    pub fn project_read_model(&self, value: &Value) -> Result<Value> {
        CompactPresentationProjection::new(self.generation.index(), self.generation.retained_peer())
            .project(value)
    }

    pub fn into_parts(self) -> (SearchQueryResult, VerifiedIndex) {
        (self.query, self.generation.into_index())
    }
}

pub struct SearchApplicationReadModelInput<'input> {
    pub commands: &'input [SearchResultCommands],
    pub freshness_mode: &'input str,
    pub generated_at: &'input str,
    pub semantic_fallback_code: Option<&'input str>,
    pub semantic_fallback_detail: Option<&'input str>,
    pub metrics: SearchRenderMetrics<'input>,
}

pub fn execute_search<Generation, Semantic>(
    request: SearchApplicationRequest,
    generation_port: &mut Generation,
    semantic_port: &Semantic,
) -> std::result::Result<SearchApplicationResult, SearchApplicationError<Generation::Error>>
where
    Generation: GenerationReadPort,
    Semantic: HistorySemanticPort,
{
    let retained_peer = request.retained_peer_read();
    let SearchApplicationRequest {
        plan,
        generation_target,
        active_session,
        ..
    } = request;
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target: generation_target,
            retained_peer,
        },
    )
    .map_err(SearchApplicationError::Generation)?;
    let query_started = Instant::now();
    let query = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .search(plan, active_session.as_ref(), semantic_port)
        .map_err(SearchApplicationError::Query)?;
    let query_duration = query_started.elapsed();
    let copied_lineage_read_models = query
        .copied_lineages
        .iter()
        .map(copied_lineage_read_model)
        .collect::<Result<Vec<_>>>()
        .map_err(SearchExecutionError::from)
        .map_err(SearchApplicationError::Query)?;
    Ok(SearchApplicationResult {
        generation,
        query,
        copied_lineage_read_models,
        query_duration,
    })
}
