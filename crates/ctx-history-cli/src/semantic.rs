//! Daemon-owned semantic and refresh adapters exposed without final CLI types.

pub use ctx_daemon_cli::{
    coordinate_source_backed_refresh, pin_active_verified_generation,
    wait_for_daemon_query_service, PinnedSourceBackedGeneration, SemanticNotReady,
    SemanticQueryAdapter, SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode,
    SourceBackedRefreshObservation,
};
