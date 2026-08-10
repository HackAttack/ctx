mod hydration;
mod query;
mod semantic_port;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
#[cfg(test)]
use ctx_history_index::EventSearchCandidate;
use ctx_history_index::{EventSearchFilters, VerifiedIndex, SEARCH_COPIED_EVENT_LINEAGE_POLICY};
use serde_json::Value;

use crate::{
    analytics::{
        count_bucket, duration_bucket, text_length_bucket, RefreshStatus, SearchTelemetry,
    },
    config,
    local_usage::{CliUsage, ResultObservationAction, SearchContextObservation},
    output::{print_json, JsonOutputFormat},
    semantic::{
        coordinate_source_backed_refresh, wait_for_daemon_query_service,
        PinnedSourceBackedGeneration, SourceBackedRefreshDaemonUnavailable,
        SourceBackedRefreshMode, SourceBackedRefreshObservation,
    },
    ui::{
        canonical_human_output_bytes, diagnostic, Action, Diagnostic, DiagnosticLevel, Document,
        RenderContext, Ui,
    },
    RefreshArg, SearchArgs, SearchBackendArg,
};

use super::{
    compact_presentation::{reference_needs_retained_peer, CompactPresentation},
    compact_ref::CompactRefResolver,
    copied_lineage::copied_lineage_value,
    render::{
        pretty_json_stdout_bytes, render_search_document, render_search_not_ready_document,
        search_json_with_lineages,
    },
    shared::{index_root, render_active_generation_race, ActiveGenerationRaceCommand},
};

use hydration::presentations_for_search_hits;
pub(in crate::commands::source_index) use hydration::SearchPresentation;
#[cfg(test)]
pub(super) use hydration::{
    presentations_for_search_hits_with_budget, SearchPresentationHydrationBudget,
    SearchPresentationRetentionBudgetExceeded, SEARCH_PRESENTATION_HYDRATION_BUDGET,
    SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
};
use query::index_search_filters_with_refs;
#[cfg(test)]
pub(super) use query::source_search_request;
pub(super) use query::NormalizedSearchQuery;
pub(crate) use query::SourceSearchRequest;
#[cfg(test)]
pub(super) use query::{index_search_filters, resolve_source_search_backend};
use query::{
    normalize_search_request, resolve_source_search_backend_with_port, unsupported_semantic_scope,
    validate_search_request,
};
pub(crate) use semantic_port::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticCapability,
};

const MAX_USAGE_CONTEXT_EVENTS_PER_SESSION: usize = 256;
pub(super) const MISSING_INDEX_ERROR: &str =
    "the Core index does not exist; retry with daemon refresh enabled";
const QUEUED_WITHOUT_GENERATION_ERROR: &str =
    "daemon source refresh was queued but no published generation exists; retry with --refresh wait";

#[derive(Debug)]
pub(super) enum SourceSearchFailure {
    Semantic(HistorySemanticError),
    SourceUnavailable,
    GenerationChanged,
    GenerationAuthority(ctx_history_refresh::GenerationQueryAuthorityError),
    Other(anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum McpSearchError {
    #[error("source-backed semantic search is not ready ({code}): {detail}")]
    SemanticNotReady {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
    #[error("{detail}")]
    SemanticFailed { detail: String },
    #[error("source_unavailable")]
    SourceUnavailable,
    #[error(
        "History changed while ctx was opening the searchable generation. Retry the same request."
    )]
    GenerationChanged,
    #[error(transparent)]
    GenerationAuthority(ctx_history_refresh::GenerationQueryAuthorityError),
    #[error("{detail}")]
    Application { detail: String },
}

impl SourceSearchFailure {
    pub(super) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Semantic(error) => semantic_error_into_anyhow(error),
            Self::SourceUnavailable => {
                anyhow::Error::new(ctx_history_refresh::MissingActiveGeneration)
            }
            Self::GenerationChanged => {
                anyhow::Error::new(ctx_history_index::IndexError::ConcurrentGenerationChange)
            }
            Self::GenerationAuthority(error) => anyhow::Error::new(error),
            Self::Other(error) => error,
        }
    }

    fn into_mcp(self) -> McpSearchError {
        match self {
            Self::Semantic(HistorySemanticError::NotReady {
                code,
                detail,
                retryable,
            }) => McpSearchError::SemanticNotReady {
                code,
                detail,
                retryable,
            },
            Self::Semantic(HistorySemanticError::Failed { detail }) => {
                McpSearchError::SemanticFailed { detail }
            }
            Self::SourceUnavailable => McpSearchError::SourceUnavailable,
            Self::GenerationChanged => McpSearchError::GenerationChanged,
            Self::GenerationAuthority(error) => McpSearchError::GenerationAuthority(error),
            Self::Other(error) => McpSearchError::Application {
                detail: error.to_string(),
            },
        }
    }
}

fn semantic_error_into_anyhow(error: HistorySemanticError) -> anyhow::Error {
    match error {
        HistorySemanticError::NotReady { code, detail, .. } => {
            anyhow::Error::new(crate::semantic::SemanticNotReady::new(code, detail))
        }
        HistorySemanticError::Failed { detail } => anyhow::anyhow!(detail),
    }
}

impl std::fmt::Display for SourceSearchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Semantic(error) => std::fmt::Display::fmt(error, formatter),
            Self::SourceUnavailable => std::fmt::Display::fmt(
                &ctx_history_refresh::MissingActiveGeneration,
                formatter,
            ),
            Self::GenerationChanged => formatter.write_str(
                "History changed while ctx was opening the searchable generation. Retry the same request.",
            ),
            Self::GenerationAuthority(error) => std::fmt::Display::fmt(error, formatter),
            Self::Other(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for SourceSearchFailure {}

impl From<HistorySemanticError> for SourceSearchFailure {
    fn from(error: HistorySemanticError) -> Self {
        Self::Semantic(error)
    }
}

impl From<anyhow::Error> for SourceSearchFailure {
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<ctx_history_refresh::MissingActiveGeneration>() {
            Ok(_) => return Self::SourceUnavailable,
            Err(error) => error,
        };
        let error = match error.downcast::<ctx_history_index::IndexError>() {
            Ok(error) => return Self::from(error),
            Err(error) => error,
        };
        match error.downcast::<ctx_history_refresh::GenerationQueryAuthorityError>() {
            Ok(error) => Self::GenerationAuthority(error),
            Err(error) => Self::Other(error),
        }
    }
}

impl From<std::io::Error> for SourceSearchFailure {
    fn from(error: std::io::Error) -> Self {
        Self::Other(anyhow::Error::new(error))
    }
}

impl From<ctx_history_index::IndexError> for SourceSearchFailure {
    fn from(error: ctx_history_index::IndexError) -> Self {
        match error {
            ctx_history_index::IndexError::ConcurrentGenerationChange => Self::GenerationChanged,
            other => Self::Other(anyhow::Error::new(other)),
        }
    }
}

impl From<ctx_history_query::SearchExecutionError> for SourceSearchFailure {
    fn from(error: ctx_history_query::SearchExecutionError) -> Self {
        match error {
            ctx_history_query::SearchExecutionError::Semantic(error) => Self::Semantic(error),
            ctx_history_query::SearchExecutionError::Index(error) => Self::from(error),
            ctx_history_query::SearchExecutionError::Application(error) => Self::from(error),
        }
    }
}

type SourceSearchResult<T> = std::result::Result<T, SourceSearchFailure>;

pub(super) use ctx_history_query::{SearchCollection, SearchHit, SemanticFallbackDiagnostics};
#[cfg(test)]
pub(super) use ctx_history_query::{SearchEventMetadata, SearchResultWindow};

pub(super) struct RefreshOutcome {
    pub(super) pin: PinnedSourceBackedGeneration,
    pub(super) status: &'static str,
    pub(super) source_count: usize,
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let human_output = args.format != JsonOutputFormat::Json;
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(&data_root);
    let result = run_search_inner(
        args,
        data_root.clone(),
        telemetry,
        local_usage,
        ui,
        &semantic_port,
    )
    .map_err(SourceSearchFailure::into_anyhow);
    render_search_error(result, human_output, &data_root, ui)
}

fn run_search_inner<P: HistorySemanticPort>(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
    semantic_port: &P,
) -> SourceSearchResult<()> {
    let config = config::AppConfig::load(&data_root)?;
    let mut request = query::source_search_request(&args);
    normalize_search_request(&mut request)?;
    let requested_backend =
        resolve_source_search_backend_with_port(&request, &config, semantic_port)?;
    request.backend = Some(requested_backend);
    request.semantic_enabled = config.semantic_search_enabled();
    request.semantic_daemon_enabled = config.daemon.enabled;
    let semantic_weight = request.semantic_weight;
    let json_output = args.format == JsonOutputFormat::Json;
    if request.refresh == RefreshArg::Background
        && request.semantic_enabled
        && semantic_port.capability() == SemanticCapability::Available
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
        && unsupported_semantic_scope(&request).is_none()
        && !(requested_backend == SearchBackendArg::Hybrid && semantic_weight == 0.0)
    {
        wait_for_daemon_query_service(&data_root, Duration::from_secs(3));
    }
    let refresh_started = Instant::now();
    let refresh = refresh_for_search(&request, &data_root)?;
    let initial_refresh_duration = refresh_started.elapsed();
    telemetry.refresh_mode = Some(match request.refresh {
        RefreshArg::Background => crate::analytics::RefreshMode::Background,
        RefreshArg::Off => crate::analytics::RefreshMode::Off,
        RefreshArg::Wait => crate::analytics::RefreshMode::Wait,
    });

    let query_started = Instant::now();
    let (value, collection, index, refresh_status, refresh_source_count) =
        search_pinned_generation(
            &request,
            &data_root,
            semantic_weight,
            refresh,
            semantic_port,
        )?;
    if !json_output {
        if let Some(fallback) = collection.semantic_fallback.as_ref() {
            let warning = render_semantic_fallback_warning(ui.stderr_context(), fallback);
            ui.write_stderr(&warning)?;
        }
    }
    let query_duration = query_started.elapsed();
    telemetry.refresh_duration = Some(duration_bucket(initial_refresh_duration));
    telemetry.refresh_status = Some(RefreshStatus::from_safe_summary(refresh_status));
    telemetry.refresh_source_count = Some(count_bucket(refresh_source_count as u64));
    telemetry.query_duration = Some(duration_bucket(query_duration));
    telemetry.query_length = Some(text_length_bucket(request.query.chars().count()));
    telemetry.query_term_count = Some(count_bucket(
        request
            .query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .count() as u64,
    ));
    telemetry.backend_requested = Some(crate::observability_product::search_backend(
        collection.requested_backend,
    ));
    telemetry.backend_effective = Some(crate::observability_product::search_backend(
        collection.effective_backend,
    ));
    telemetry.has_indexed_content_after = Some(index.document_count() > 0);
    telemetry.result_count = Some(count_bucket(collection.result_window.hits.len() as u64));
    telemetry.citation_count = Some(count_bucket(collection.result_window.hits.len() as u64));
    telemetry.zero_result = Some(collection.result_window.hits.is_empty());

    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let result_count = results.len();
    let search_context = if config.local_usage.enabled {
        search_context_observation(&value, &collection, &index)
    } else {
        SearchContextObservation::unavailable()
    };
    let render_started = Instant::now();
    let compact_value = (!json_output)
        .then(|| CompactPresentation::open(&index, &index_root(&data_root))?.project(&value))
        .transpose()?;
    let render_value = compact_value.as_ref().unwrap_or(&value);
    let output_bytes = if args.format == JsonOutputFormat::Json {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render_search_document(render_value, args.verbose, ui.stdout_context());
        let output_bytes = canonical_human_output_bytes(|context| {
            render_search_document(render_value, args.verbose, context)
        });
        ui.write_stdout(&document)?;
        output_bytes
    };
    telemetry.render_duration = Some(duration_bucket(render_started.elapsed()));
    local_usage.set_result_observation(ResultObservationAction::Search, result_count, 0, 0);
    local_usage.set_search_context_observation(search_context);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

pub(super) fn render_search_error<T>(
    result: Result<T>,
    human_output: bool,
    data_root: &Path,
    ui: &mut Ui,
) -> Result<T> {
    let result = render_active_generation_race(
        result,
        !human_output,
        ActiveGenerationRaceCommand::Search,
        ui,
    );
    match result {
        Ok(value) => Ok(value),
        Err(error) if human_output && search_index_is_not_ready(data_root, &error) => {
            let document = render_search_not_ready_document(ui.stderr_context());
            ui.write_stderr(&document)?;
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

fn search_index_is_not_ready(data_root: &Path, error: &anyhow::Error) -> bool {
    let missing_generation = error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            MISSING_INDEX_ERROR | QUEUED_WITHOUT_GENERATION_ERROR
        )
    });
    let root = index_root(data_root);
    let active_generation_missing = VerifiedIndex::active_generation_id(&root)
        .ok()
        .flatten()
        .is_none();
    missing_generation
        || (active_generation_missing
            && error
                .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
                .is_some())
}

pub(super) fn render_semantic_fallback_warning(
    context: &RenderContext,
    fallback: &SemanticFallbackDiagnostics,
) -> Document {
    let (summary, detail, action) = match fallback.code {
        "semantic_disabled" => (
            "Semantic search is unavailable",
            "Keyword search was used because semantic search is disabled.",
            "ctx setup --semantic",
        ),
        "semantic_content_scope_unsupported" | "semantic_event_type_unsupported" => (
            "Semantic search does not support this filter",
            "Keyword search was used because this content filter is lexical-only.",
            "ctx search \"<term>\" --backend lexical",
        ),
        _ => (
            "Semantic search is unavailable",
            "Keyword search was used because semantic retrieval did not complete.",
            "ctx doctor",
        ),
    };
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary,
            detail: Some(detail),
            fields: &[],
            action: Some(Action { command: action }),
        },
    )
}

#[cfg(test)]
pub(crate) fn mcp_search(
    request: SourceSearchRequest,
    data_root: &Path,
) -> std::result::Result<(Value, SearchContextObservation), McpSearchError> {
    let config = config::AppConfig::load(data_root)
        .map_err(|error| SourceSearchFailure::from(error).into_mcp())?;
    mcp_search_with_compact(request, data_root, &config)
        .map(|(value, observation, _)| (value, observation))
}

pub(crate) fn mcp_search_with_compact(
    mut request: SourceSearchRequest,
    data_root: &Path,
    config: &config::AppConfig,
) -> std::result::Result<(Value, SearchContextObservation, Value), McpSearchError> {
    normalize_mcp_search_request(&mut request)?;
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(data_root);
    mcp_search_inner(request, data_root, config, &semantic_port)
        .map_err(SourceSearchFailure::into_mcp)
}

pub(crate) fn normalize_mcp_search_request(
    request: &mut SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    normalize_search_request(request).map_err(|error| SourceSearchFailure::from(error).into_mcp())
}

fn mcp_search_inner<P: HistorySemanticPort>(
    mut request: SourceSearchRequest,
    data_root: &Path,
    config: &config::AppConfig,
    semantic_port: &P,
) -> SourceSearchResult<(Value, SearchContextObservation, Value)> {
    request.backend = Some(resolve_source_search_backend_with_port(
        &request,
        config,
        semantic_port,
    )?);
    request.semantic_enabled = config.semantic_search_enabled();
    request.semantic_daemon_enabled = config.daemon.enabled;
    let semantic_weight = request.semantic_weight;
    let refresh = refresh_for_search(&request, data_root)?;
    let (value, collection, index, _, _) =
        search_pinned_generation(&request, data_root, semantic_weight, refresh, semantic_port)?;
    let observation = if config.local_usage.enabled {
        search_context_observation(&value, &collection, &index)
    } else {
        SearchContextObservation::unavailable()
    };
    let compact_value =
        CompactPresentation::open(&index, &index_root(data_root))?.project(&value)?;
    Ok((value, observation, compact_value))
}

pub(crate) fn validate_explicit_semantic_scope(
    request: &SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    if request.backend == Some(SearchBackendArg::Semantic) {
        if let Some(not_ready) = unsupported_semantic_scope(request) {
            return Err(SourceSearchFailure::Semantic(not_ready).into_mcp());
        }
    }
    Ok(())
}

pub(super) fn search_context_observation(
    value: &Value,
    collection: &SearchCollection,
    index: &VerifiedIndex,
) -> SearchContextObservation {
    if collection.result_window.hits.is_empty() {
        return SearchContextObservation::unavailable();
    }
    let Some(delivered_context_bytes) =
        value
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| {
                results.iter().try_fold(0_usize, |total, result| {
                    total.checked_add(result.get("snippet")?.as_str()?.len())
                })
            })
    else {
        return SearchContextObservation::unavailable();
    };
    let session_ids = collection
        .result_window
        .hits
        .iter()
        .map(|hit| hit.event.session_id)
        .collect::<BTreeSet<_>>();
    let mut matched_normalized_session_bytes = 0_usize;
    for session_id in session_ids {
        let Ok(Some(session_bytes)) = index.core_content_bytes_for_session_if_bounded(
            session_id,
            MAX_USAGE_CONTEXT_EVENTS_PER_SESSION,
        ) else {
            return SearchContextObservation::unavailable();
        };
        let Some(total) = matched_normalized_session_bytes.checked_add(session_bytes) else {
            return SearchContextObservation::unavailable();
        };
        matched_normalized_session_bytes = total;
    }
    SearchContextObservation::complete(delivered_context_bytes, matched_normalized_session_bytes)
        .unwrap_or_else(SearchContextObservation::unavailable)
}

pub(super) fn refresh_for_search(
    request: &SourceSearchRequest,
    data_root: &Path,
) -> SourceSearchResult<RefreshOutcome> {
    refresh_for_search_with(request, data_root, coordinate_source_backed_refresh)
}

pub(super) fn refresh_for_search_with<Coordinate>(
    request: &SourceSearchRequest,
    data_root: &Path,
    coordinate: Coordinate,
) -> SourceSearchResult<RefreshOutcome>
where
    Coordinate: FnOnce(&Path, SourceBackedRefreshMode) -> Result<SourceBackedRefreshObservation>,
{
    validate_search_request(request)?;
    let mode = source_backed_refresh_mode(request.refresh);
    let observation = match coordinate(data_root, mode) {
        Ok(observation) => observation,
        Err(error) if mode == SourceBackedRefreshMode::Background => {
            // Background refresh may report an uncertified empty generation as
            // unavailable. At the query gateway, preserve the stricter typed
            // R1 authority error instead of replacing it with refresh state.
            if let Err(authority_error) = crate::semantic::pin_active_verified_generation(data_root)
            {
                if let Ok(authority_error) =
                    authority_error.downcast::<ctx_history_refresh::GenerationQueryAuthorityError>()
                {
                    return Err(SourceSearchFailure::GenerationAuthority(authority_error));
                }
            }
            return Err(SourceSearchFailure::from(error));
        }
        Err(error) => return Err(SourceSearchFailure::from(error)),
    };
    if observation.mode != mode {
        return Err(anyhow!(
            "source-backed refresh coordinator returned mode {:?} for requested mode {:?}",
            observation.mode,
            mode
        )
        .into());
    }
    let status = match mode {
        SourceBackedRefreshMode::Off => "existing_generation",
        SourceBackedRefreshMode::Background if observation.daemon_available => "daemon_background",
        SourceBackedRefreshMode::Background => "daemon_unavailable",
        SourceBackedRefreshMode::Wait => "completed",
    };
    Ok(RefreshOutcome {
        pin: observation.pin,
        status,
        source_count: observation.source_count,
    })
}

pub(super) fn source_backed_refresh_mode(refresh: RefreshArg) -> SourceBackedRefreshMode {
    match refresh {
        RefreshArg::Off => SourceBackedRefreshMode::Off,
        RefreshArg::Background => SourceBackedRefreshMode::Background,
        RefreshArg::Wait => SourceBackedRefreshMode::Wait,
    }
}

fn search_pinned_generation<P: HistorySemanticPort>(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    refresh: RefreshOutcome,
    semantic_port: &P,
) -> SourceSearchResult<(Value, SearchCollection, VerifiedIndex, &'static str, usize)> {
    let RefreshOutcome {
        pin,
        status,
        source_count,
    } = refresh;
    let (value, collection, index) = search_existing_generation_with_port(
        request,
        pin.into_index(),
        data_root,
        semantic_weight,
        status,
        source_count,
        semantic_port,
    )?;
    Ok((value, collection, index, status, source_count))
}

#[cfg(test)]
pub(super) fn search_existing_generation(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
) -> Result<(Value, SearchCollection, VerifiedIndex)> {
    search_existing_generation_with_port(
        request,
        index,
        data_root,
        semantic_weight,
        refresh_status,
        refresh_source_count,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
    )
    .map_err(SourceSearchFailure::into_anyhow)
}

fn search_existing_generation_with_port<P: HistorySemanticPort>(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
    semantic_port: &P,
) -> SourceSearchResult<(Value, SearchCollection, VerifiedIndex)> {
    validate_search_request(request)?;
    let input_references = CompactPresentation::open_if_needed(
        &index,
        &index_root(data_root),
        request
            .session
            .as_deref()
            .is_some_and(reference_needs_retained_peer),
    )?;
    let pinned_references = CompactRefResolver::new(&index, None);
    let input_resolver = input_references
        .as_ref()
        .map(CompactPresentation::resolver)
        .unwrap_or(pinned_references);
    let filters = index_search_filters_with_refs(request, &index, &input_resolver)?;
    let query_started = Instant::now();
    let collection = collect_search_hits_with_port(
        request,
        &index,
        data_root,
        semantic_weight,
        &filters,
        semantic_port,
    )?;
    let query_duration = query_started.elapsed();
    let presentations = presentations_for_search_hits(
        &index,
        &collection.result_window.hits,
        &NormalizedSearchQuery::from_request(request),
    )?;
    let copied_lineages = collection
        .result_window
        .hits
        .iter()
        .map(|hit| {
            copied_lineage_value(
                &index,
                hit.event.event_id,
                SEARCH_COPIED_EVENT_LINEAGE_POLICY,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let value = search_json_with_lineages(
        request,
        data_root,
        &index,
        &collection,
        &filters,
        &presentations,
        &copied_lineages,
        refresh_status,
        refresh_source_count,
        query_duration,
    )?;
    Ok((value, collection, index))
}

#[cfg(test)]
pub(super) fn collect_search_hits_with_backend(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    filters: &EventSearchFilters,
) -> Result<SearchCollection> {
    collect_search_hits_with_port(
        request,
        index,
        data_root,
        semantic_weight,
        filters,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
    )
    .map_err(SourceSearchFailure::into_anyhow)
}

fn collect_search_hits_with_port<P: HistorySemanticPort>(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    _data_root: &Path,
    semantic_weight: f32,
    filters: &EventSearchFilters,
    semantic_port: &P,
) -> SourceSearchResult<SearchCollection> {
    let mut planned = request.clone();
    planned.semantic_weight = semantic_weight;
    ctx_history_query::collect_search_hits(&planned, index, filters, semantic_port)
        .map_err(SourceSearchFailure::from)
}

#[cfg(test)]
pub(super) fn collect_search_hits_with_backend_using<SemanticSearch>(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    filters: &EventSearchFilters,
    mut semantic_search: SemanticSearch,
) -> Result<SearchCollection>
where
    SemanticSearch: FnMut(
        &VerifiedIndex,
        &Path,
        &str,
        &EventSearchFilters,
        usize,
    ) -> Result<(Vec<EventSearchCandidate>, Value)>,
{
    let mut planned = request.clone();
    planned.semantic_weight = semantic_weight;
    ctx_history_query::collect_search_hits_using(
        &planned,
        index,
        filters,
        |query, filters, candidate_limit| {
            semantic_search(index, data_root, query, filters, candidate_limit)
                .map(|(candidates, diagnostics)| HistorySemanticBatch {
                    candidates,
                    diagnostics,
                })
                .map_err(|error| HistorySemanticError::failed(format!("{error:#}")))
        },
    )
    .map_err(SourceSearchFailure::from)
    .map_err(SourceSearchFailure::into_anyhow)
}

#[cfg(test)]
pub(super) use ctx_history_query::shape_search_result_window;
