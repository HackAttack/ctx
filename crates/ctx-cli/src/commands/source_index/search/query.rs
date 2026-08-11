#[cfg(test)]
use anyhow::Result;
use ctx_history_index::SearchContentScope;
#[cfg(test)]
use ctx_history_index::{EventSearchFilters, VerifiedIndex};

use crate::{
    cli::{CliSearchBackendArg, ContentScopeArg},
    config, SearchArgs, SearchBackendArg,
};

#[cfg(test)]
use super::semantic_error_into_anyhow;
use super::semantic_port::{SemanticAvailability, SemanticReason};

pub(super) use ctx_history_read_application::unsupported_semantic_scope;
pub(crate) use ctx_history_read_application::{
    NormalizedSearchQuery, SearchPolicy, SearchRequest as SourceSearchRequest,
};

pub(crate) fn source_search_request(args: &SearchArgs) -> SourceSearchRequest {
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
    }
}

pub(in crate::commands::source_index) fn source_search_policy(
    config: &config::AppConfig,
) -> SearchPolicy {
    let semantic_enabled = config.semantic_search_enabled();
    let semantic = if !semantic_enabled {
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled)
    } else if !crate::semantic::semantic_query_service_supported() {
        SemanticAvailability::Unavailable(SemanticReason::PlatformUnsupported)
    } else if !config.daemon.enabled {
        SemanticAvailability::Unavailable(SemanticReason::ExecutionUnavailable)
    } else {
        SemanticAvailability::Available
    };
    SearchPolicy {
        default_backend: if semantic_enabled {
            SearchBackendArg::Hybrid
        } else {
            SearchBackendArg::Lexical
        },
        semantic,
    }
}

#[cfg(test)]
pub(in crate::commands::source_index) fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    ctx_history_read_application::resolve_search_backend(request, source_search_policy(config))
        .map_err(semantic_error_into_anyhow)
}

#[cfg(test)]
pub(in crate::commands::source_index) fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    ctx_history_read_application::search_filters(request, index, None)
}
