use anyhow::Result;
use ctx_history_index::{EventSearchFilters, SearchContentScope, VerifiedIndex};

use crate::{
    cli::{CliSearchBackendArg, ContentScopeArg},
    commands::search::CliRefreshArg,
    config, RefreshArg, SearchArgs, SearchBackendArg,
};

use super::super::compact_ref::CompactRefResolver;
#[cfg(test)]
use super::semantic_error_into_anyhow;
use super::semantic_port::{HistorySemanticError, HistorySemanticPort};

pub(super) use ctx_history_query::{
    normalize_search_request, unsupported_semantic_scope, validate_search_request,
};
pub(crate) use ctx_history_query::{NormalizedSearchQuery, SearchRequest as SourceSearchRequest};

pub(in crate::commands::source_index) fn source_search_request(
    args: &SearchArgs,
) -> SourceSearchRequest {
    SourceSearchRequest {
        query: args.query.clone().unwrap_or_default(),
        terms: args.term.clone(),
        limit: args.limit,
        provider: args.provider.map(|provider| provider.capture_provider()),
        history_source: args.history_source.clone(),
        provider_key: args.provider_key.clone(),
        source_id: args.source_id.clone(),
        source_format: args.source_format.clone(),
        workspace: args.workspace.clone(),
        since: args.since.clone(),
        primary_only: args.primary_only,
        include_subagents: args.include_subagents,
        content_scope: match args.content_scope.unwrap_or(ContentScopeArg::All) {
            ContentScopeArg::All => SearchContentScope::All,
            ContentScopeArg::Transcript => SearchContentScope::Transcript,
            ContentScopeArg::Calls => SearchContentScope::Calls,
            ContentScopeArg::Outputs => SearchContentScope::Outputs,
        },
        event_type: args.event_type.clone(),
        file: args.file.clone(),
        session: args.session.clone(),
        events: args.events || args.session.is_some(),
        include_current_session: args.include_current_session,
        backend: args.backend.map(|backend| match backend {
            CliSearchBackendArg::Hybrid => SearchBackendArg::Hybrid,
            CliSearchBackendArg::Lexical => SearchBackendArg::Lexical,
            CliSearchBackendArg::Semantic => SearchBackendArg::Semantic,
        }),
        semantic_weight: args.semantic_weight,
        semantic_enabled: false,
        semantic_daemon_enabled: false,
        refresh: match args.refresh {
            CliRefreshArg::Background => RefreshArg::Background,
            CliRefreshArg::Off => RefreshArg::Off,
            CliRefreshArg::Wait => RefreshArg::Wait,
        },
    }
}

pub(in crate::commands::source_index) fn resolve_source_search_backend_with_port<
    P: HistorySemanticPort,
>(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
    semantic_port: &P,
) -> std::result::Result<SearchBackendArg, HistorySemanticError> {
    let mut planned = request.clone();
    planned.semantic_enabled = config.semantic_search_enabled();
    planned.semantic_daemon_enabled = config.daemon.enabled;
    ctx_history_query::resolve_search_backend(&planned, semantic_port)
}

#[cfg(test)]
pub(in crate::commands::source_index) fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    resolve_source_search_backend_with_port(
        request,
        config,
        &crate::semantic::SemanticQueryAdapter::new(std::path::Path::new("")),
    )
    .map_err(semantic_error_into_anyhow)
}

#[cfg(test)]
pub(in crate::commands::source_index) fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    ctx_history_query::search_filters(request, index)
}

pub(in crate::commands::source_index) fn index_search_filters_with_refs(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
    references: &CompactRefResolver<'_>,
) -> Result<EventSearchFilters> {
    ctx_history_query::search_filters_with_refs(request, index, references)
}
