#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use ctx_history_index::{EventSearchFilters, VerifiedIndex};

use crate::{config, SearchBackend as HistorySearchBackend, SearchContentScope, SearchRequest};

#[cfg(test)]
use super::semantic_error_into_anyhow;
use super::semantic_port::{SemanticAvailability, SemanticReason};

pub(super) use ctx_history_read_application::unsupported_semantic_scope;
pub use ctx_history_read_application::{
    NormalizedSearchQuery, SearchBackend, SearchPolicy, SearchRequest as SourceSearchRequest,
};

pub fn source_search_request(args: &SearchRequest) -> SourceSearchRequest {
    SourceSearchRequest {
        query: args.query.clone().unwrap_or_default(),
        terms: args.terms.clone(),
        limit: args.limit,
        provider: args.provider.clone().map(|provider| match provider {
            crate::HistoryProvider::Native(value) => value
                .parse()
                .unwrap_or(ctx_history_core::CaptureProvider::Unknown),
            crate::HistoryProvider::Custom => ctx_history_core::CaptureProvider::Custom,
        }),
        history_source: args.history_source.clone(),
        provider_key: args.provider_key.clone(),
        source_id: args.source_id.clone(),
        source_format: args.source_format.clone(),
        workspace: args.workspace.clone(),
        since: args.since.clone(),
        primary_only: args.primary_only,
        include_subagents: args.include_subagents,
        content_scope: match args.content_scope {
            SearchContentScope::All => ctx_history_index::SearchContentScope::All,
            SearchContentScope::Transcript => ctx_history_index::SearchContentScope::Transcript,
            SearchContentScope::Calls => ctx_history_index::SearchContentScope::Calls,
            SearchContentScope::Outputs => ctx_history_index::SearchContentScope::Outputs,
        },
        event_type: args.event_type.clone(),
        file: args.file.clone(),
        session: args.session.clone(),
        events: args.events || args.session.is_some(),
        include_current_session: args.include_current_session,
        backend: args.backend.map(|backend| match backend {
            HistorySearchBackend::Hybrid => SearchBackend::Hybrid,
            HistorySearchBackend::Lexical => SearchBackend::Lexical,
            HistorySearchBackend::Semantic => SearchBackend::Semantic,
        }),
        semantic_weight: args.semantic_weight,
    }
}

pub(in crate::source_index) fn source_search_policy(config: &config::AppConfig) -> SearchPolicy {
    let semantic_enabled = config.semantic_search_enabled();
    let semantic = if !semantic_enabled {
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled)
    } else if !ctx_daemon_cli::semantic_query_service_supported() {
        SemanticAvailability::Unavailable(SemanticReason::PlatformUnsupported)
    } else if !config.daemon.enabled {
        SemanticAvailability::Unavailable(SemanticReason::ExecutionUnavailable)
    } else {
        SemanticAvailability::Available
    };
    SearchPolicy {
        default_backend: if semantic_enabled {
            SearchBackend::Hybrid
        } else {
            SearchBackend::Lexical
        },
        semantic,
    }
}

#[cfg(test)]
pub(in crate::source_index) fn resolve_source_search_backend(
    request: &SourceSearchRequest,
    config: &config::AppConfig,
) -> Result<SearchBackend> {
    ctx_history_read_application::resolve_search_backend(request, source_search_policy(config))
        .map_err(semantic_error_into_anyhow)
}

#[cfg(test)]
pub(in crate::source_index) fn index_search_filters(
    request: &SourceSearchRequest,
    index: &VerifiedIndex,
) -> Result<EventSearchFilters> {
    ctx_history_read_application::search_filters(request, index, None)
}
