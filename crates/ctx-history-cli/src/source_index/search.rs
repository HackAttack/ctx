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
#[cfg(test)]
use ctx_history_index::EventSearchFilters;
use ctx_history_index::VerifiedIndex;
use serde_json::Value;

use crate::{
    cli::SearchArgs,
    config,
    local_usage::{CliUsage, ResultObservationAction, SearchContextObservation},
    output::{print_json, JsonOutputFormat},
    semantic::coordinate_source_backed_refresh,
    ui::{
        canonical_human_output_bytes, diagnostic, Action, Diagnostic, DiagnosticLevel, Document,
        RenderContext, Ui,
    },
    HistoryCliConfig, RefreshMode, SearchExecutionObservation, SearchRefreshStatus,
};
use ctx_daemon_cli::{
    wait_for_daemon_query_service, PinnedSourceBackedGeneration,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode, SourceBackedRefreshObservation,
};

use super::{
    compact_presentation::generation_read,
    render::{
        pretty_json_stdout_bytes, render_search_document, render_search_not_ready_document,
        search_json_with_lineages,
    },
    shared::{
        externalize_query_error, index_root, render_active_generation_race,
        ActiveGenerationRaceCommand,
    },
};
use ctx_history_read_application::SearchBackend;

pub(in crate::source_index) use hydration::SearchPresentation;
#[cfg(test)]
pub(super) use hydration::{
    presentations_for_search_hits_with_budget, SearchPresentationHydrationBudget,
    SearchPresentationRetentionBudgetExceeded, SEARCH_PRESENTATION_HYDRATION_BUDGET,
    SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
};
pub(super) use query::NormalizedSearchQuery;
pub use query::SourceSearchRequest;
#[cfg(test)]
pub(super) use query::{index_search_filters, resolve_source_search_backend};
use query::{source_search_policy, unsupported_semantic_scope};
#[cfg(test)]
use semantic_port::HistorySemanticBatch;
pub(crate) use semantic_port::{
    HistorySemanticError, HistorySemanticPort, SemanticAvailability, SemanticReason,
};

const MAX_USAGE_CONTEXT_EVENTS_PER_SESSION: usize = 256;
type RefreshArg = RefreshMode;
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
pub enum McpSearchError {
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
                reason,
                detail,
                retryable,
            }) => McpSearchError::SemanticNotReady {
                code: semantic_reason_code(reason),
                detail: semantic_external_detail(reason, &detail),
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
        HistorySemanticError::NotReady { reason, detail, .. } => {
            anyhow::Error::new(crate::semantic::SemanticNotReady::new(
                semantic_reason_code(reason),
                semantic_external_detail(reason, &detail),
            ))
        }
        HistorySemanticError::Failed { detail } => anyhow::anyhow!(detail),
    }
}

pub(super) const fn semantic_reason_code(reason: SemanticReason) -> &'static str {
    match reason {
        SemanticReason::PolicyDisabled => "semantic_disabled",
        SemanticReason::PlatformUnsupported => "semantic_unsupported",
        SemanticReason::ExecutionUnavailable => "semantic_daemon_disabled",
        SemanticReason::ContentScopeUnsupported => "semantic_content_scope_unsupported",
        SemanticReason::EventTypeUnsupported => "semantic_event_type_unsupported",
        SemanticReason::QueryServiceUnavailable => "semantic_query_service_unavailable",
        SemanticReason::StoreUnavailable => "semantic_store_unavailable",
        SemanticReason::StoreMissing => "semantic_store_missing",
        SemanticReason::GenerationUnreadable => "semantic_generation_unreadable",
        SemanticReason::GenerationNotAcknowledged => "semantic_generation_not_acknowledged",
        SemanticReason::GenerationReceiptMismatch => "semantic_generation_receipt_mismatch",
        SemanticReason::ProjectionEventMismatch => "semantic_projection_event_mismatch",
        SemanticReason::Adapter(code) => code,
    }
}

fn semantic_external_detail(reason: SemanticReason, detail: &str) -> String {
    match reason {
        SemanticReason::PolicyDisabled => "semantic search is disabled. Set [search] semantic = true in ctx config to enable local semantic search".to_owned(),
        SemanticReason::PlatformUnsupported => "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical".to_owned(),
        SemanticReason::ExecutionUnavailable => "local semantic search requires automatic indexing. Run `ctx index mode auto`, set [search] semantic = false, or use --backend lexical".to_owned(),
        SemanticReason::ContentScopeUnsupported => format!("{detail}; use --backend lexical or choose --content-scope all|transcript"),
        SemanticReason::EventTypeUnsupported => format!("{detail}; use --backend lexical or remove --event-type"),
        _ => detail.to_owned(),
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
            Err(error) => Self::Other(externalize_query_error(error)),
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

impl From<ctx_history_read_application::SearchExecutionError> for SourceSearchFailure {
    fn from(error: ctx_history_read_application::SearchExecutionError) -> Self {
        match error {
            ctx_history_read_application::SearchExecutionError::Semantic(error) => {
                Self::Semantic(error)
            }
            ctx_history_read_application::SearchExecutionError::Index(error) => Self::from(error),
            ctx_history_read_application::SearchExecutionError::Application(error) => {
                Self::from(error)
            }
        }
    }
}

fn application_search_failure(
    error: ctx_history_read_application::SearchApplicationError<anyhow::Error>,
) -> SourceSearchFailure {
    use ctx_history_read_application::{GenerationReadError, SearchApplicationError};

    match error {
        SearchApplicationError::Generation(GenerationReadError::Port(error)) => {
            SourceSearchFailure::from(error)
        }
        SearchApplicationError::Generation(GenerationReadError::Authority(error)) => {
            SourceSearchFailure::Other(anyhow::Error::new(error))
        }
        SearchApplicationError::Query(error) => SourceSearchFailure::from(error),
    }
}

type SourceSearchResult<T> = std::result::Result<T, SourceSearchFailure>;

pub(super) use ctx_history_read_application::{SearchCollection, SemanticFallbackDiagnostics};
#[cfg(test)]
pub(super) use ctx_history_read_application::{SearchEventMetadata, SearchHit, SearchResultWindow};

pub(super) struct RefreshOutcome {
    pub(super) pin: PinnedSourceBackedGeneration,
    pub(super) status: &'static str,
    pub(super) source_count: usize,
}

struct SearchRefreshContext<'a> {
    mode: RefreshArg,
    status: &'a str,
    source_count: usize,
}

pub fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    config: HistoryCliConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
    observe_query: impl FnOnce(SearchExecutionObservation),
) -> Result<SearchExecutionObservation> {
    let human_output = args.format != JsonOutputFormat::Json;
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(&data_root);
    let result = run_search_inner(
        args,
        data_root.clone(),
        config,
        local_usage,
        ui,
        &semantic_port,
        observe_query,
    )
    .map_err(SourceSearchFailure::into_anyhow);
    render_search_error(result, human_output, &data_root, ui)
}

fn run_search_inner<P: HistorySemanticPort>(
    args: SearchArgs,
    data_root: PathBuf,
    config: HistoryCliConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
    semantic_port: &P,
    observe_query: impl FnOnce(SearchExecutionObservation),
) -> SourceSearchResult<SearchExecutionObservation> {
    let config = config::AppConfig::from_snapshot(config);
    let request = crate::SearchRequest::from(args);
    let refresh_mode = request.refresh;
    let json_output = request.format == crate::OutputFormat::Json;
    let verbose = request.verbose;
    let policy = source_search_policy(&config);
    let plan = ctx_history_read_application::plan_search(request.into(), policy)?;
    let request = plan.request();
    let requested_backend = request.backend.unwrap_or(policy.default_backend);
    let semantic_weight = request.semantic_weight;
    let query_length = request.query.chars().count();
    let query_terms = request
        .query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .count();
    if refresh_mode == RefreshArg::Background
        && policy.semantic == SemanticAvailability::Available
        && matches!(
            requested_backend,
            SearchBackend::Semantic | SearchBackend::Hybrid
        )
        && unsupported_semantic_scope(request).is_none()
        && !(requested_backend == SearchBackend::Hybrid && semantic_weight == 0.0)
    {
        wait_for_daemon_query_service(&data_root, Duration::from_secs(3));
    }
    let refresh_started = Instant::now();
    let refresh = refresh_for_search(request, refresh_mode, &data_root)?;
    let initial_refresh_duration = refresh_started.elapsed();
    let query_started = Instant::now();
    let (value, application, refresh_status, refresh_source_count) = search_pinned_generation(
        plan,
        &data_root,
        refresh_mode,
        refresh,
        !json_output,
        semantic_port,
        super::detected_active_session(),
    )?;
    let collection = &application.query().collection;
    let index = application.index();
    if !json_output {
        if let Some(fallback) = collection.semantic_fallback.as_ref() {
            let warning = render_semantic_fallback_warning(ui.stderr_context(), fallback);
            ui.write_stderr(&warning)?;
        }
    }
    let query_duration = query_started.elapsed();
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let result_count = results.len();
    let search_context = if config.local_usage.enabled {
        search_context_observation(&value, collection, index)
    } else {
        SearchContextObservation::unavailable()
    };
    let mut observation = SearchExecutionObservation {
        refresh_mode,
        refresh_status: match refresh_status {
            "existing_generation" => SearchRefreshStatus::ExistingGeneration,
            "daemon_background" => SearchRefreshStatus::DaemonBackground,
            "daemon_unavailable" => SearchRefreshStatus::DaemonUnavailable,
            _ => SearchRefreshStatus::Completed,
        },
        refresh_source_count: refresh_source_count as u64,
        refresh_duration: initial_refresh_duration,
        query_duration,
        render_duration: None,
        backend_requested: collection.requested_backend,
        backend_effective: collection.effective_backend,
        result_count: result_count as u64,
        citation_count: collection.result_window.hits.len() as u64,
        zero_result: collection.result_window.hits.is_empty(),
        has_indexed_content_after: index.document_count() > 0,
        query_length: query_length as u64,
        query_term_count: query_terms as u64,
    };
    observe_query(observation);

    let render_started = Instant::now();
    let compact_value = (!json_output)
        .then(|| application.project_read_model(&value))
        .transpose()?;
    let render_value = compact_value.as_ref().unwrap_or(&value);
    let output_bytes = if json_output {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render_search_document(render_value, verbose, ui.stdout_context());
        let output_bytes = canonical_human_output_bytes(|context| {
            render_search_document(render_value, verbose, context)
        });
        ui.write_stdout(&document)?;
        output_bytes
    };
    observation.render_duration = Some(render_started.elapsed());
    local_usage.set_result_observation(ResultObservationAction::Search, result_count, 0);
    local_usage.set_search_context_observation(search_context);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(observation)
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
    let (summary, detail, action) = match fallback.reason {
        Some(SemanticReason::PolicyDisabled) => (
            "Semantic search is unavailable",
            "Keyword search was used because semantic search is disabled.",
            "ctx setup --semantic",
        ),
        Some(SemanticReason::ContentScopeUnsupported | SemanticReason::EventTypeUnsupported) => (
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
    config: HistoryCliConfig,
) -> std::result::Result<(Value, SearchContextObservation), McpSearchError> {
    mcp_search_with_compact(request, data_root, config)
        .map(|(value, observation, _)| (value, observation))
}

pub fn mcp_search_with_compact(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
) -> std::result::Result<(Value, SearchContextObservation, Value), McpSearchError> {
    let config = config::AppConfig::from_snapshot(config);
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(data_root);
    mcp_search_inner(request, data_root, &config, &semantic_port)
        .map_err(SourceSearchFailure::into_mcp)
}

pub fn normalize_mcp_search_request(
    request: &mut SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    ctx_history_read_application::normalize_search_request(request)
        .map_err(|error| SourceSearchFailure::from(error).into_mcp())
}

fn mcp_search_inner<P: HistorySemanticPort>(
    request: SourceSearchRequest,
    data_root: &Path,
    config: &config::AppConfig,
    semantic_port: &P,
) -> SourceSearchResult<(Value, SearchContextObservation, Value)> {
    let plan = ctx_history_read_application::plan_search(request, source_search_policy(config))?;
    let refresh = refresh_for_search(plan.request(), RefreshArg::Off, data_root)?;
    let (value, application, _, _) = search_pinned_generation(
        plan,
        data_root,
        RefreshArg::Off,
        refresh,
        true,
        semantic_port,
        None,
    )?;
    let observation = if config.local_usage.enabled {
        search_context_observation(&value, &application.query().collection, application.index())
    } else {
        SearchContextObservation::unavailable()
    };
    let compact_value = application.project_read_model(&value)?;
    Ok((value, observation, compact_value))
}

pub fn validate_explicit_semantic_scope(
    request: &SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    if request.backend == Some(SearchBackend::Semantic) {
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
    refresh: RefreshArg,
    data_root: &Path,
) -> SourceSearchResult<RefreshOutcome> {
    refresh_for_search_with(
        request,
        refresh,
        data_root,
        coordinate_source_backed_refresh,
    )
}

pub(super) fn refresh_for_search_with<Coordinate>(
    request: &SourceSearchRequest,
    refresh: RefreshArg,
    data_root: &Path,
    coordinate: Coordinate,
) -> SourceSearchResult<RefreshOutcome>
where
    Coordinate: FnOnce(&Path, SourceBackedRefreshMode) -> Result<SourceBackedRefreshObservation>,
{
    ctx_history_read_application::validate_search_request(request)?;
    let mode = source_backed_refresh_mode(refresh);
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
    plan: ctx_history_read_application::PlannedSearch,
    data_root: &Path,
    refresh_mode: RefreshArg,
    refresh: RefreshOutcome,
    compact_projection: bool,
    semantic_port: &P,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
) -> SourceSearchResult<(
    Value,
    ctx_history_read_application::SearchApplicationResult,
    &'static str,
    usize,
)> {
    let RefreshOutcome {
        pin,
        status,
        source_count,
    } = refresh;
    let (value, application) = search_existing_generation_with_port(
        plan,
        pin.into_index(),
        data_root,
        SearchRefreshContext {
            mode: refresh_mode,
            status,
            source_count,
        },
        compact_projection,
        semantic_port,
        active_session,
    )?;
    Ok((value, application, status, source_count))
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
    let policy = ctx_history_read_application::SearchPolicy {
        default_backend: request.backend.unwrap_or(SearchBackend::Lexical),
        semantic: SemanticAvailability::Available,
    };
    let mut request = request.clone();
    request.semantic_weight = semantic_weight;
    let plan = ctx_history_read_application::plan_search(request, policy)
        .map_err(SourceSearchFailure::from)
        .map_err(SourceSearchFailure::into_anyhow)?;
    search_existing_generation_with_port(
        plan,
        index,
        data_root,
        SearchRefreshContext {
            mode: RefreshArg::Off,
            status: refresh_status,
            source_count: refresh_source_count,
        },
        false,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
        None,
    )
    .map(|(value, application)| {
        let (query, index) = application.into_parts();
        (value, query.collection, index)
    })
    .map_err(SourceSearchFailure::into_anyhow)
}

fn search_existing_generation_with_port<P: HistorySemanticPort>(
    plan: ctx_history_read_application::PlannedSearch,
    index: VerifiedIndex,
    data_root: &Path,
    refresh: SearchRefreshContext<'_>,
    compact_projection: bool,
    semantic_port: &P,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
) -> SourceSearchResult<(Value, ctx_history_read_application::SearchApplicationResult)> {
    let mut index = Some(index);
    let mut generation_port = |request: &ctx_history_read_application::GenerationReadRequest| {
        generation_read(
            index.take().expect("generation port is invoked once"),
            &index_root(data_root),
            request,
        )
    };
    let result = ctx_history_read_application::execute_search(
        ctx_history_read_application::SearchApplicationRequest {
            plan,
            generation_target: ctx_history_read_application::GenerationReadTarget::Active,
            compact_projection,
            active_session,
        },
        &mut generation_port,
        semantic_port,
    )
    .map_err(application_search_failure)?;
    let query = result.query();
    let value = search_json_with_lineages(
        &query.request,
        data_root,
        result.index(),
        &query.collection,
        &query.filters,
        &query.presentations,
        result.copied_lineage_read_models(),
        refresh.mode,
        refresh.status,
        refresh.source_count,
        result.query_duration(),
    )?;
    Ok((value, result))
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

#[cfg(test)]
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
    ctx_history_read_application::collect_search_hits(
        &planned,
        index,
        filters,
        SemanticAvailability::Available,
        semantic_port,
    )
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
    ctx_history_read_application::collect_search_hits_using(
        &planned,
        index,
        filters,
        SemanticAvailability::Available,
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
pub(super) use ctx_history_read_application::shape_search_result_window;
