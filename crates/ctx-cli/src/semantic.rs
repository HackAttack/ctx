#[cfg(test)]
fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

#[allow(unused_imports)]
pub(crate) use ctx_semantic_model::{
    prepare_platform_semantic_acceleration, semantic_managed_model_snapshot_dir,
    semantic_native_accelerator_target, semantic_provisioning_coreml_asset_matches,
    semantic_provisioning_model_contract_matches, semantic_provisioning_model_path_count,
    semantic_provisioning_model_path_matches, semantic_query_service_supported,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    SemanticNativeAcceleratorTarget, SemanticOrtModelVariant,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_semantic_model::{
    semantic_model_cache_available, semantic_model_key, SemanticDaemonCpuFallbackRequired,
    SemanticDaemonModelAcquisition, SemanticModelLoadDeferred, SharedSemanticRuntime,
    SEMANTIC_DIMENSIONS,
};
mod model_config;
pub(crate) use model_config::{semantic_runtime_cache_dir, semantic_worker_cache_dir};
mod runtime_limits;
pub(crate) use ctx_semantic_index::SemanticNotReady;
#[allow(unused_imports)]
pub(crate) use runtime_limits::{DAEMON_IDLE_EXIT_SECONDS_CAP, SEMANTIC_WORKER_BATCH_MAX};
mod query_adapter;
pub(crate) use query_adapter::SemanticQueryAdapter;
mod query_service;
pub(crate) use query_service::wait_for_daemon_query_service;
mod daemon;
mod paths_status;
pub(crate) use daemon::run_daemon_command;
pub(crate) mod daemon_service_ports;
mod daemon_status;
mod daemon_supervisor;
mod source_status;
pub(crate) use source_status::source_epoch_status_report;
mod source_backed_pro_catch_up;
pub(crate) use ctx_daemon_service::wait_for_completed_generation as wait_for_source_backed_pro_generation;
pub(crate) use ctx_daemon_service::{
    helper_recheck_targets as source_backed_pro_recheck_targets,
    publish_helper_recheck_intent as publish_source_backed_pro_recheck,
    wake_helper_recheck as wake_source_backed_pro_recheck,
};
pub(crate) use source_backed_pro_catch_up::cancel_core_finalization_generation_lease;
mod source_backed_refresh_coordinator;
pub(crate) use source_backed_refresh_coordinator::{
    coordinate_import_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, pin_active_verified_generation,
    published_explicit_source_relocation_authority, PinnedSourceBackedGeneration, RefreshStatus,
    SourceBackedCurrentSourceProgress, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshMode, SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
};
mod daemon_autostart;
#[allow(unused_imports)]
pub(crate) use daemon_autostart::{
    autostart_daemon_and_wait, autostart_daemon_for_setup_and_wait,
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, maybe_autostart_daemon,
    replacement_helper_owns_daemon_handoff, DaemonHandoff, DaemonSetupHandoff,
    DaemonUpgradeHandoff,
};
mod health_search;
#[cfg(test)]
mod tests;
