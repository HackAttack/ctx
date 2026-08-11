mod compact_presentation;
mod copied_lineage;
mod locate;
mod render;
mod search;
mod shared;
mod show;

pub(crate) use compact_presentation::open_generation_read;
pub use copied_lineage::copied_lineage_summary;
pub use locate::run_locate;
#[cfg(test)]
use search::mcp_search;
pub use search::source_search_request;
pub use search::{
    mcp_search_with_compact, normalize_mcp_search_request, run_search,
    run_search_with_observations, validate_explicit_semantic_scope, McpSearchError,
    SourceSearchRequest,
};
pub use shared::generation_query_authority_error_json;
#[cfg(test)]
pub(crate) use show::{mcp_show_event, mcp_show_event_with_compact};
pub use show::{
    mcp_show_event_application, mcp_show_session_application, run_show, ShowApplicationError,
};

#[cfg(test)]
pub(crate) fn event_origin_json(origin: &ctx_history_core::EventOrigin) -> serde_json::Value {
    ctx_history_read_application::event_origin_json(origin)
}

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use ctx_history_core::CaptureProvider;
#[cfg(test)]
use ctx_history_index::{CoreEventRecord, EventRecord, EventSearchCandidate};

#[cfg(test)]
use crate::{
    config,
    semantic::{
        PinnedSourceBackedGeneration, SemanticNotReady, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
    },
    RefreshArg, SearchBackendArg,
};

#[cfg(test)]
use render::{enforce_json_output_limit, pretty_json_stdout_bytes, stdout_body_bytes};
#[cfg(test)]
use search::{
    collect_search_hits_with_backend, collect_search_hits_with_backend_using, index_search_filters,
    refresh_for_search, refresh_for_search_with, search_context_observation,
    search_existing_generation, shape_search_result_window, source_backed_refresh_mode,
};
#[cfg(test)]
use shared::{externalize_query_error, index_root, open_index};
#[cfg(test)]
use show::resolve_show_session;

include!("source_index/tests.rs");
