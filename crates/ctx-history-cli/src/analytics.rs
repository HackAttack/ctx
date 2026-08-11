//! Observation drafts only. Final event construction and delivery remain in
//! the final binary.

pub use ctx_client_observability::analytics::{
    count_bucket, duration_bucket, text_length_bucket, LocateTelemetry, RefreshMode, RefreshStatus,
    SearchTelemetry, ShowTelemetry,
};

pub const fn search_backend(
    value: ctx_history_read_application::SearchBackend,
) -> ctx_client_observability::analytics::SearchBackend {
    match value {
        ctx_history_read_application::SearchBackend::Hybrid => {
            ctx_client_observability::analytics::SearchBackend::Hybrid
        }
        ctx_history_read_application::SearchBackend::Lexical => {
            ctx_client_observability::analytics::SearchBackend::Lexical
        }
        ctx_history_read_application::SearchBackend::Semantic => {
            ctx_client_observability::analytics::SearchBackend::Semantic
        }
    }
}
