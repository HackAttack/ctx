use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventOrigin, EventType, SessionRelationshipKind};
use ctx_history_index_query::{
    AgentScope, EventRecord, EventSearchCandidate, EventSearchFilters, ExcludedSessionTree,
    IndexError, SearchContentScope, VerifiedIndex, LEXICAL_QUERY_LIMITS, MAX_LEXICAL_QUERY_RESULTS,
};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    normalize_source_identity_filters, parse_since_filter, resolve_session_with_refs,
    CompactRefResolver, HistorySemanticBatch, HistorySemanticError, HistorySemanticPort,
    HistorySemanticQuery, SourceIdentityFilterArgs, SourceIdentityFilters,
};

const LEGACY_ACTIVE_SESSION_PROVIDER_ENV: &str = "CODEX_THREAD_ID";
const LEGACY_ACTIVE_SESSION_PROVIDER: CaptureProvider = CaptureProvider::Codex;
const MAX_SESSION_DIVERSITY_CANDIDATES: usize = 64 * 1024;
const MIN_CANDIDATE_BATCH: usize = 256;
const CANDIDATE_OVERSAMPLE: usize = 8;
const SOURCE_FUSION_CANDIDATES: usize = 1_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

impl fmt::Display for SearchBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchBackend {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "hybrid" => Ok(Self::Hybrid),
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            other => Err(format!(
                "invalid search backend {other:?}; expected hybrid, lexical, or semantic"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRefreshMode {
    Background,
    Off,
    Wait,
}

impl SearchRefreshMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

impl fmt::Display for SearchRefreshMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchRefreshMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "background" => Ok(Self::Background),
            "off" => Ok(Self::Off),
            "wait" => Ok(Self::Wait),
            other => Err(format!(
                "invalid search refresh mode {other:?}; expected background, off, or wait"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<CaptureProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub include_subagents: bool,
    pub content_scope: SearchContentScope,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub include_current_session: bool,
    pub backend: Option<SearchBackend>,
    pub semantic_weight: f32,
    pub semantic_enabled: bool,
    pub semantic_daemon_enabled: bool,
    pub refresh: SearchRefreshMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSearchQuery {
    positional: Option<String>,
    terms: Vec<String>,
    alternatives: Vec<String>,
    display: String,
}

impl NormalizedSearchQuery {
    pub fn from_request(request: &SearchRequest) -> Self {
        let positional = normalized_query_alternative(&request.query);
        let terms = request
            .terms
            .iter()
            .filter_map(|term| normalized_query_alternative(term))
            .collect::<Vec<_>>();
        let alternatives = positional
            .iter()
            .chain(terms.iter())
            .cloned()
            .collect::<Vec<_>>();
        let display = alternatives.join(" OR ");
        Self {
            positional,
            terms,
            alternatives,
            display,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.alternatives.is_empty()
    }

    pub fn texts(&self) -> Vec<&str> {
        self.alternatives.iter().map(String::as_str).collect()
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn shell_arguments(&self) -> String {
        let mut arguments = Vec::with_capacity(self.alternatives.len().saturating_mul(2));
        if let Some(positional) = self.positional.as_deref() {
            arguments.push(shell_quote_arg(positional));
        }
        for term in &self.terms {
            arguments.push(format!("--term={}", shell_quote_arg(term)));
        }
        arguments.join(" ")
    }
}

fn normalized_query_alternative(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '/' | ':' | '@')
        })
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn validate_search_request(request: &SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    if request
        .workspace
        .as_deref()
        .is_some_and(|workspace| workspace.trim().is_empty())
    {
        return Err(anyhow!("query filter workspace is empty"));
    }
    if request
        .file
        .as_ref()
        .is_some_and(|file| file.to_str().is_some_and(|file| file.trim().is_empty()))
    {
        return Err(anyhow!("query filter file is empty"));
    }
    let source_identity = normalized_request_source_identity_filters(request)?;
    if !source_identity.is_empty()
        && request
            .provider
            .is_some_and(|provider| provider != CaptureProvider::Custom)
    {
        return Err(anyhow!(
            "custom history source filters can only be combined with --provider custom"
        ));
    }
    let has_query = !NormalizedSearchQuery::from_request(request).is_empty();
    if !has_query && request.file.is_none() {
        return Err(anyhow!("source-backed search needs a non-empty text query"));
    }
    if !has_query
        && request
            .backend
            .is_some_and(|backend| backend != SearchBackend::Lexical)
    {
        return Err(anyhow!(
            "semantic and hybrid search need a non-empty text query"
        ));
    }
    Ok(())
}

pub fn normalize_search_request(request: &mut SearchRequest) -> Result<()> {
    validate_lexical_query_limits(request)?;
    if request.workspace.is_some() {
        request.workspace = normalized_optional_text(request.workspace.as_deref())
            .map(Some)
            .ok_or_else(|| anyhow!("query filter workspace is empty"))?;
    }
    if let Some(file) = request.file.as_ref().and_then(|file| file.to_str()) {
        let file = normalized_optional_text(Some(file))
            .ok_or_else(|| anyhow!("query filter file is empty"))?;
        request.file = Some(PathBuf::from(file));
    }
    Ok(())
}

fn validate_lexical_query_limits(request: &SearchRequest) -> Result<()> {
    let positional = (!request.query.is_empty()).then_some(request.query.as_str());
    LEXICAL_QUERY_LIMITS.validate_texts(
        positional
            .into_iter()
            .chain(request.terms.iter().map(String::as_str)),
    )?;
    Ok(())
}

fn normalized_request_source_identity_filters(
    request: &SearchRequest,
) -> Result<SourceIdentityFilters> {
    normalize_source_identity_filters(SourceIdentityFilterArgs {
        history_source: request.history_source.clone(),
        provider_key: request.provider_key.clone(),
        source_id: request.source_id.clone(),
        source_format: request.source_format.clone(),
    })
}

pub fn resolve_search_backend<P: HistorySemanticPort>(
    request: &SearchRequest,
    semantic_port: &P,
) -> std::result::Result<SearchBackend, HistorySemanticError> {
    if request.backend.is_none()
        && NormalizedSearchQuery::from_request(request).is_empty()
        && request.file.is_some()
    {
        return Ok(SearchBackend::Lexical);
    }
    if request.backend == Some(SearchBackend::Semantic) {
        if let Some(not_ready) = unsupported_semantic_scope(request) {
            return Err(not_ready);
        }
    }
    match request.backend {
        Some(SearchBackend::Semantic) if !request.semantic_enabled => {
            Err(HistorySemanticError::not_ready(
                "semantic_disabled",
                "semantic search is disabled. Set [search] semantic = true in ctx config to enable local semantic search",
                false,
            ))
        }
        Some(SearchBackend::Semantic)
            if semantic_port.capability() == crate::SemanticCapability::Unavailable =>
        {
            Err(HistorySemanticError::not_ready(
                "semantic_unsupported",
                "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical",
                false,
            ))
        }
        Some(SearchBackend::Semantic) if !request.semantic_daemon_enabled => {
            Err(HistorySemanticError::not_ready(
                "semantic_daemon_disabled",
                "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical",
                false,
            ))
        }
        Some(value) => Ok(value),
        None if request.semantic_enabled => Ok(SearchBackend::Hybrid),
        None => Ok(SearchBackend::Lexical),
    }
}

pub fn unsupported_semantic_scope(request: &SearchRequest) -> Option<HistorySemanticError> {
    let content_scope = match request.content_scope {
        SearchContentScope::Calls => Some("calls"),
        SearchContentScope::Outputs => Some("outputs"),
        SearchContentScope::All | SearchContentScope::Transcript => None,
    };
    if let Some(content_scope) = content_scope {
        return Some(HistorySemanticError::not_ready(
            "semantic_content_scope_unsupported",
            format!(
                "semantic retrieval does not support content scope '{content_scope}'; use --backend lexical or choose --content-scope all|transcript"
            ),
            false,
        ));
    }

    let event_type = request
        .event_type
        .as_deref()
        .and_then(|value| value.parse::<EventType>().ok())
        .filter(|event_type| *event_type != EventType::Message)?;
    Some(HistorySemanticError::not_ready(
        "semantic_event_type_unsupported",
        format!(
            "semantic retrieval does not support event type '{}'; use --backend lexical or remove --event-type",
            event_type.as_str()
        ),
        false,
    ))
}

pub fn search_filters(
    request: &SearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    let references = CompactRefResolver::new(index, None);
    search_filters_with_refs(request, index, &references)
}

pub fn search_filters_with_refs(
    request: &SearchRequest,
    index: &VerifiedIndex,
    references: &CompactRefResolver<'_>,
) -> Result<EventSearchFilters> {
    let source_identity = normalized_request_source_identity_filters(request)?;
    let session_id = request
        .session
        .as_deref()
        .map(|id| {
            resolve_session_with_refs(references, id).map(|session| session.session_id.as_uuid())
        })
        .transpose()?;
    let event_type = request
        .event_type
        .as_deref()
        .map(|value| {
            value
                .parse::<EventType>()
                .map(|event_type| event_type.as_str().to_owned())
                .map_err(|error| anyhow!("{error}"))
        })
        .transpose()?;
    let since_unix_ms = request
        .since
        .as_deref()
        .map(parse_since_filter)
        .transpose()?
        .map(|since| since.timestamp_millis());
    let exclude_session_tree = (!request.include_current_session && session_id.is_none())
        .then(|| std::env::var(LEGACY_ACTIVE_SESSION_PROVIDER_ENV).ok())
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|provider_session_id| excluded_active_session_tree(index, provider_session_id))
        .transpose()?;
    Ok(EventSearchFilters {
        session_id,
        provider: request
            .provider
            .or_else(|| (!source_identity.is_empty()).then_some(CaptureProvider::Custom))
            .map(|provider| provider.as_str().to_owned()),
        history_source: source_identity.history_source,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        workspace: normalized_optional_text(request.workspace.as_deref()),
        since_unix_ms,
        content_scope: request.content_scope,
        event_type,
        agent_scope: if request.primary_only || !request.include_subagents {
            AgentScope::Primary
        } else {
            AgentScope::All
        },
        file: request
            .file
            .as_ref()
            .and_then(|path| normalized_optional_text(Some(&path.display().to_string()))),
        exclude_session_tree,
        ..EventSearchFilters::default()
    })
}

fn normalized_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn excluded_active_session_tree(
    index: &VerifiedIndex,
    provider_session_id: String,
) -> Result<ExcludedSessionTree> {
    let sessions = index.sessions_by_provider_session_id(
        &provider_session_id,
        Some(LEGACY_ACTIVE_SESSION_PROVIDER.as_str()),
    )?;
    let session_id = match sessions.as_slice() {
        [session] => Some(session.root_session_id.as_uuid()),
        [first, second] if first.root_session_id == second.root_session_id => {
            Some(first.root_session_id.as_uuid())
        }
        _ => None,
    };
    Ok(ExcludedSessionTree {
        provider: LEGACY_ACTIVE_SESSION_PROVIDER.as_str().to_owned(),
        provider_session_id,
        session_id,
    })
}

#[derive(Debug, Error)]
pub enum SearchExecutionError {
    #[error(transparent)]
    Semantic(#[from] HistorySemanticError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Application(#[from] anyhow::Error),
}

pub type SearchExecutionResult<T> = std::result::Result<T, SearchExecutionError>;

#[derive(Debug)]
pub struct SearchCollection {
    pub result_window: SearchResultWindow,
    pub candidate_pool: usize,
    pub candidate_pool_truncated: bool,
    pub requested_backend: SearchBackend,
    pub effective_backend: SearchBackend,
    pub semantic_weight: f32,
    pub semantic_status: &'static str,
    pub semantic_fallback: Option<SemanticFallbackDiagnostics>,
    pub semantic_diagnostics: Option<Value>,
}

#[derive(Debug)]
pub struct SearchResultWindow {
    pub limit: usize,
    pub hits: Vec<SearchHit>,
    pub more_available: bool,
}

#[derive(Debug, Clone)]
pub struct SemanticFallbackDiagnostics {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub event: SearchEventMetadata,
    pub score: f32,
    pub more_matches_in_session: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEventMetadata {
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Uuid,
    pub session_relationship: SessionRelationshipKind,
    pub event_origin: EventOrigin,
    pub provider: String,
    pub source_format: String,
    pub provider_session_id: Option<String>,
    pub branch: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
}

impl From<&EventRecord> for SearchEventMetadata {
    fn from(event: &EventRecord) -> Self {
        Self {
            event_id: event.event_id.as_uuid(),
            session_id: event.session_id.as_uuid(),
            parent_session_id: event.parent_session_id.map(|id| id.as_uuid()),
            root_session_id: event.root_session_id.as_uuid(),
            session_relationship: event.session_relationship,
            event_origin: event.event_origin.clone(),
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
            event_type: event.event_type.clone(),
            role: event.role.clone(),
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
        }
    }
}

pub fn collect_search_hits<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic_port: &P,
) -> SearchExecutionResult<SearchCollection> {
    let prepared = prepare_semantic_search(request, index, filters)?;
    let (requested_backend, normalized_query) = match prepared {
        PreparedSemanticSearch::Complete(collection) => return Ok(collection),
        PreparedSemanticSearch::Query {
            requested_backend,
            normalized_query,
        } => (requested_backend, normalized_query),
    };

    match semantic_port.begin_query(index) {
        Ok(mut semantic_query) => collect_prepared_semantic_search(
            request,
            index,
            filters,
            requested_backend,
            normalized_query,
            |query, filters, candidate_limit| {
                semantic_query.candidates(query, filters, candidate_limit)
            },
        ),
        Err(error) => collect_prepared_semantic_search(
            request,
            index,
            filters,
            requested_backend,
            normalized_query,
            |_, _, _| Err(error.clone()),
        ),
    }
}

pub fn collect_search_hits_using<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    semantic_search: SemanticSearch,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
        &str,
        &EventSearchFilters,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    let prepared = prepare_semantic_search(request, index, filters)?;
    let (requested_backend, normalized_query) = match prepared {
        PreparedSemanticSearch::Complete(collection) => return Ok(collection),
        PreparedSemanticSearch::Query {
            requested_backend,
            normalized_query,
        } => (requested_backend, normalized_query),
    };
    collect_prepared_semantic_search(
        request,
        index,
        filters,
        requested_backend,
        normalized_query,
        semantic_search,
    )
}

enum PreparedSemanticSearch {
    Complete(SearchCollection),
    Query {
        requested_backend: SearchBackend,
        normalized_query: NormalizedSearchQuery,
    },
}

fn prepare_semantic_search(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
) -> SearchExecutionResult<PreparedSemanticSearch> {
    let requested_backend = request.backend.unwrap_or(SearchBackend::Lexical);
    let semantic_weight = request.semantic_weight;
    if !semantic_weight.is_finite() || !(0.0..=1.0).contains(&semantic_weight) {
        return Err(anyhow!("semantic weight must be finite and between 0.0 and 1.0").into());
    }
    if requested_backend == SearchBackend::Lexical
        || (requested_backend == SearchBackend::Hybrid && semantic_weight == 0.0)
    {
        let normalized_query = NormalizedSearchQuery::from_request(request);
        let queries = normalized_query.texts();
        let mut collection =
            collect_lexical_search_hits(index, &queries, request.limit, request.events, filters)?;
        collection.requested_backend = requested_backend;
        collection.semantic_weight = 0.0;
        return Ok(PreparedSemanticSearch::Complete(collection));
    }
    if let Some(not_ready) = unsupported_semantic_scope(request) {
        if requested_backend == SearchBackend::Semantic {
            return Err(not_ready.into());
        }
        return lexical_fallback(
            request,
            index,
            filters,
            requested_backend,
            not_ready,
            "unsupported",
        )
        .map(PreparedSemanticSearch::Complete);
    }
    if !request.semantic_enabled || !request.semantic_daemon_enabled {
        let not_ready = if request.semantic_enabled {
            HistorySemanticError::not_ready(
                "semantic_daemon_disabled",
                "local semantic retrieval is unavailable because the ctx daemon is disabled",
                false,
            )
        } else {
            HistorySemanticError::not_ready(
                "semantic_disabled",
                "local semantic retrieval is disabled",
                false,
            )
        };
        if requested_backend == SearchBackend::Semantic {
            return Err(not_ready.into());
        }
        let status = if request.semantic_enabled {
            "unavailable"
        } else {
            "disabled"
        };
        return lexical_fallback(
            request,
            index,
            filters,
            requested_backend,
            not_ready,
            status,
        )
        .map(PreparedSemanticSearch::Complete);
    }

    Ok(PreparedSemanticSearch::Query {
        requested_backend,
        normalized_query: NormalizedSearchQuery::from_request(request),
    })
}

fn lexical_fallback(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
) -> SearchExecutionResult<SearchCollection> {
    lexical_fallback_with_diagnostics(
        request,
        index,
        filters,
        requested_backend,
        not_ready,
        status,
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn lexical_fallback_with_diagnostics(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    not_ready: HistorySemanticError,
    status: &'static str,
    semantic_query_diagnostics: Vec<Value>,
) -> SearchExecutionResult<SearchCollection> {
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let queries = normalized_query.texts();
    let mut collection =
        collect_lexical_search_hits(index, &queries, request.limit, request.events, filters)?;
    let fallback = semantic_fallback_diagnostics(&not_ready);
    collection.requested_backend = requested_backend;
    collection.effective_backend = SearchBackend::Lexical;
    collection.semantic_weight = if status == "unsupported" {
        0.0
    } else {
        request.semantic_weight
    };
    collection.semantic_status = status;
    collection.semantic_fallback = Some(fallback.clone());
    collection.semantic_diagnostics = Some(json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
        "fallback": {
            "code": fallback.code,
            "detail": fallback.detail,
        },
    }));
    Ok(collection)
}

#[allow(clippy::too_many_arguments)]
fn collect_prepared_semantic_search<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    requested_backend: SearchBackend,
    normalized_query: NormalizedSearchQuery,
    mut semantic_search: SemanticSearch,
) -> SearchExecutionResult<SearchCollection>
where
    SemanticSearch: FnMut(
        &str,
        &EventSearchFilters,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    let queries = normalized_query.texts();
    let mut semantic_by_event = BTreeMap::<Uuid, EventSearchCandidate>::new();
    let mut semantic_query_diagnostics = Vec::with_capacity(queries.len());
    for query in &queries {
        let HistorySemanticBatch {
            candidates,
            diagnostics,
        } = match semantic_search(query, filters, SOURCE_FUSION_CANDIDATES) {
            Ok(value) => value,
            Err(error) if requested_backend == SearchBackend::Hybrid => {
                return lexical_fallback_with_diagnostics(
                    request,
                    index,
                    filters,
                    requested_backend,
                    error,
                    "unavailable",
                    semantic_query_diagnostics,
                )
            }
            Err(error) => return Err(error.into()),
        };
        semantic_query_diagnostics.push(json!({
            "query": query,
            "diagnostics": diagnostics,
        }));
        for candidate in candidates {
            semantic_by_event
                .entry(candidate.event.event_id.as_uuid())
                .and_modify(|existing| {
                    if candidate.score > existing.score {
                        *existing = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
    }
    let mut semantic_candidates = semantic_by_event.into_values().collect::<Vec<_>>();
    semantic_candidates.sort_by(|left, right| {
        right.score.total_cmp(&left.score).then_with(|| {
            left.event
                .event_id
                .as_uuid()
                .cmp(&right.event.event_id.as_uuid())
        })
    });
    semantic_candidates.truncate(SOURCE_FUSION_CANDIDATES);
    let semantic_diagnostics = json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
    });

    let candidates = if requested_backend == SearchBackend::Semantic {
        semantic_candidates
    } else {
        let lexical_candidates = index.search_event_candidates_any_with_filters(
            &queries,
            filters,
            SOURCE_FUSION_CANDIDATES,
        )?;
        fuse_source_candidates(
            lexical_candidates,
            semantic_candidates,
            request.semantic_weight,
        )
    };
    let candidate_pool = candidates.len();
    let result_window =
        shape_search_result_window(candidates.iter(), request.limit, request.events);
    Ok(SearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated: candidate_pool >= SOURCE_FUSION_CANDIDATES,
        requested_backend,
        effective_backend: requested_backend,
        semantic_weight: if requested_backend == SearchBackend::Semantic {
            1.0
        } else {
            request.semantic_weight
        },
        semantic_status: "ready",
        semantic_fallback: None,
        semantic_diagnostics: Some(semantic_diagnostics),
    })
}

fn semantic_fallback_diagnostics(error: &HistorySemanticError) -> SemanticFallbackDiagnostics {
    SemanticFallbackDiagnostics {
        code: error.code(),
        detail: error.detail().to_owned(),
    }
}

fn collect_lexical_search_hits(
    index: &VerifiedIndex,
    queries: &[&str],
    limit: usize,
    event_results: bool,
    filters: &EventSearchFilters,
) -> Result<SearchCollection> {
    let document_count = usize::try_from(index.document_count()).unwrap_or(usize::MAX);
    let maximum = document_count
        .min(MAX_SESSION_DIVERSITY_CANDIDATES)
        .min(MAX_LEXICAL_QUERY_RESULTS);
    let mut candidate_limit = limit
        .saturating_mul(CANDIDATE_OVERSAMPLE)
        .max(MIN_CANDIDATE_BATCH)
        .min(maximum.max(1));

    loop {
        let candidates = if queries.is_empty() {
            index.list_event_candidates_with_filters(filters, candidate_limit)?
        } else {
            index.search_event_candidates_any_with_filters(queries, filters, candidate_limit)?
        };
        let exhausted = candidates.len() < candidate_limit || candidate_limit >= document_count;
        let result_window = shape_search_result_window(candidates.iter(), limit, event_results);
        if result_window.more_available || exhausted {
            return Ok(SearchCollection {
                result_window,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: false,
                requested_backend: SearchBackend::Lexical,
                effective_backend: SearchBackend::Lexical,
                semantic_weight: 0.0,
                semantic_status: "skipped",
                semantic_fallback: None,
                semantic_diagnostics: None,
            });
        }
        if candidate_limit >= maximum {
            return Ok(SearchCollection {
                result_window,
                candidate_pool: candidates.len(),
                candidate_pool_truncated: true,
                requested_backend: SearchBackend::Lexical,
                effective_backend: SearchBackend::Lexical,
                semantic_weight: 0.0,
                semantic_status: "skipped",
                semantic_fallback: None,
                semantic_diagnostics: None,
            });
        }
        candidate_limit = candidate_limit
            .saturating_mul(2)
            .min(maximum)
            .max(candidate_limit.saturating_add(1));
    }
}

struct SourceFusionEvidence {
    event: EventRecord,
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
}

fn fuse_source_candidates(
    lexical: Vec<EventSearchCandidate>,
    semantic: Vec<EventSearchCandidate>,
    semantic_weight: f32,
) -> Vec<EventSearchCandidate> {
    let mut evidence = BTreeMap::<Uuid, SourceFusionEvidence>::new();
    for (rank, candidate) in lexical.into_iter().enumerate() {
        evidence.insert(
            candidate.event.event_id.as_uuid(),
            SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: Some(rank.saturating_add(1)),
                semantic_rank: None,
            },
        );
    }
    for (rank, candidate) in semantic.into_iter().enumerate() {
        let semantic_rank = rank.saturating_add(1);
        evidence
            .entry(candidate.event.event_id.as_uuid())
            .and_modify(|entry| entry.semantic_rank = Some(semantic_rank))
            .or_insert(SourceFusionEvidence {
                event: candidate.event,
                lexical_rank: None,
                semantic_rank: Some(semantic_rank),
            });
    }
    let mut candidates = evidence
        .into_values()
        .map(|evidence| EventSearchCandidate {
            score: weighted_rrf_score(
                evidence.lexical_rank,
                evidence.semantic_rank,
                semantic_weight,
            ),
            event: evidence.event,
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| {
                right
                    .event
                    .occurred_at_unix_ms
                    .cmp(&left.event.occurred_at_unix_ms)
            })
            .then_with(|| right.event.event_sequence.cmp(&left.event.event_sequence))
            .then_with(|| {
                left.event
                    .event_id
                    .as_uuid()
                    .cmp(&right.event.event_id.as_uuid())
            })
    });
    candidates
}

fn weighted_rrf_score(
    lexical_rank: Option<usize>,
    semantic_rank: Option<usize>,
    semantic_weight: f32,
) -> f32 {
    let reciprocal_rank = |rank: usize| 1.0 / (60.0 + rank.max(1) as f32);
    let lexical = lexical_rank.map(reciprocal_rank).unwrap_or(0.0);
    let semantic = semantic_rank.map(reciprocal_rank).unwrap_or(0.0);
    ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic)
}

pub fn shape_search_result_window<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    limit: usize,
    event_results: bool,
) -> SearchResultWindow {
    let shape_limit = limit.saturating_add(1);
    let mut hits = if event_results {
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
        let mut positions = BTreeMap::<Uuid, usize>::new();
        let mut hits = Vec::<SearchHit>::new();
        for candidate in candidates {
            let session_id = candidate.event.session_id.as_uuid();
            if let Some(position) = positions.get(&session_id).copied() {
                if let Some(hit) = hits.get_mut(position) {
                    hit.more_matches_in_session = hit.more_matches_in_session.saturating_add(1);
                }
                continue;
            }
            if hits.len() == shape_limit {
                continue;
            }
            positions.insert(session_id, hits.len());
            hits.push(SearchHit {
                event: SearchEventMetadata::from(&candidate.event),
                score: candidate.score,
                more_matches_in_session: 0,
            });
        }
        hits
    };
    let more_available = hits.len() > limit;
    hits.truncate(limit);
    SearchResultWindow {
        limit,
        hits,
        more_available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SearchRequest {
        SearchRequest {
            query: "  first query  ".to_owned(),
            terms: vec![" second query ".to_owned(), " ".to_owned()],
            limit: 20,
            provider: None,
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            workspace: None,
            since: None,
            primary_only: false,
            include_subagents: false,
            content_scope: SearchContentScope::All,
            event_type: None,
            file: None,
            session: None,
            events: false,
            include_current_session: false,
            backend: Some(SearchBackend::Lexical),
            semantic_weight: 0.35,
            semantic_enabled: false,
            semantic_daemon_enabled: false,
            refresh: SearchRefreshMode::Off,
        }
    }

    #[test]
    fn normalized_query_preserves_or_order_and_shell_contract() {
        let query = NormalizedSearchQuery::from_request(&request());
        assert_eq!(query.texts(), vec!["first query", "second query"]);
        assert_eq!(query.display(), "first query OR second query");
        assert_eq!(
            query.shell_arguments(),
            "'first query' --term='second query'"
        );
    }

    #[test]
    fn custom_source_filter_rejects_noncustom_provider() {
        let mut request = request();
        request.history_source = Some("plugin/source".to_owned());
        request.provider = Some(CaptureProvider::Codex);
        assert_eq!(
            validate_search_request(&request).unwrap_err().to_string(),
            "custom history source filters can only be combined with --provider custom"
        );
    }

    #[test]
    fn unsupported_semantic_scope_remains_typed() {
        let mut request = request();
        request.backend = Some(SearchBackend::Semantic);
        request.content_scope = SearchContentScope::Outputs;
        let error = unsupported_semantic_scope(&request).unwrap();
        assert_eq!(error.code(), "semantic_content_scope_unsupported");
        assert!(!error.retryable());
    }

    #[test]
    fn weighted_rrf_keeps_exact_endpoint_weights() {
        assert_eq!(weighted_rrf_score(Some(1), None, 0.0), 1.0 / 61.0);
        assert_eq!(weighted_rrf_score(None, Some(1), 1.0), 1.0 / 61.0);
        assert_eq!(weighted_rrf_score(Some(1), None, 1.0), 0.0);
    }
}
