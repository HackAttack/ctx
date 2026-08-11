//! Transport-neutral application queries over one pinned Core generation.
//!
//! This crate owns query-domain parsing, selector resolution, search planning,
//! lexical/semantic ranking, and bounded result contracts. Process lifecycle,
//! refresh execution, concrete semantic services, and UI rendering remain in
//! the outer composition layer.

mod application;
mod filters;
mod list;
mod locate;
mod presentation;
mod search;
mod selector;
mod semantic;
mod show;

#[cfg(test)]
mod application_tests;

pub use application::{plan_search, PinnedHistoryQuery, PlannedSearch, SearchQueryResult};
pub use filters::{
    normalize_source_identity_filter, normalize_source_identity_filters, parse_since_filter,
    SourceIdentityFilterArgs, SourceIdentityFilterError, SourceIdentityFilters,
};
pub use list::{
    decode_event_range_cursor, encode_event_range_cursor, event_range_selection,
    parse_event_query_uuid, validated_event_limit, ListEventsError, ListEventsPageRequest,
    ListEventsRequest, ListEventsResult, DEFAULT_EVENT_QUERY_LIMIT, MAX_EVENT_QUERY_CURSOR_CHARS,
    MAX_EVENT_QUERY_LIMIT,
};
pub use locate::{LocateRequest, LocateResult};
pub use presentation::{
    presentations_for_search_hits_with_budget, search_snippet_fragment, SearchPresentation,
    SearchPresentationHydrationBudget, SearchPresentationRetentionBudgetExceeded,
    MAX_SEARCH_RESULTS, SEARCH_PRESENTATION_HYDRATION_BUDGET,
    SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES, SEARCH_SNIPPET_MAX_BYTES,
    SEARCH_SNIPPET_MAX_CHARS,
};
pub use search::{
    collect_search_hits, collect_search_hits_using, normalize_search_request,
    resolve_search_backend, search_filters, search_filters_with_refs, shape_search_result_window,
    unsupported_semantic_scope, validate_search_request, ActiveSessionExclusion,
    NormalizedSearchQuery, SearchBackend, SearchCollection, SearchEventMetadata,
    SearchExecutionError, SearchExecutionResult, SearchHit, SearchPolicy, SearchRequest,
    SearchResultWindow, SemanticFallbackDiagnostics,
};
pub use selector::{
    resolve_core_event, resolve_core_event_with_refs, resolve_session, resolve_session_with_refs,
    resolve_show_session, resolve_show_session_with_refs, validate_ctx_id,
    validate_session_selector, CompactRefMap, CompactRefNamespace, CompactRefResolveError,
    CompactRefResolver, MissingLookupError, MissingLookupKind, SelectorError,
    MAX_COMPACT_REF_HEX_LEN, MIN_COMPACT_REF_HEX_LEN,
};
pub use semantic::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticAvailability, SemanticReason,
};
pub use show::{
    ContentQueryLimitError, EncodedCoreQueryLimitError, EventWindowBudget, EventWindowLimitError,
    SessionEventMode, ShowEventRequest, ShowEventResult, ShowSessionEvent, ShowSessionPage,
    ShowSessionPageRequest,
};
