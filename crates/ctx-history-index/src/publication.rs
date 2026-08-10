mod generation;
mod manifest;
mod republish;
mod retention;
mod verification;

#[cfg(test)]
pub(crate) use ctx_history_index_generation::manifest_path;
#[cfg(test)]
pub(crate) use ctx_history_index_generation::physical_integrity_digest;
pub(crate) use ctx_history_index_generation::sync_directory;
pub(crate) use ctx_history_index_generation::{
    certify_activated_generation, reclaim_unreferenced_certifications,
    reclaim_unreferenced_manifests, scrub_and_certify_physical_integrity,
    verify_or_certify_physical_integrity,
};
pub(crate) use ctx_history_index_generation::{
    physical_integrity_audit, verify_physical_integrity, PhysicalIntegrityAudit,
};
pub(crate) use generation::{
    create_candidate_generation, lexical_index_settings, load_active_generation_pointer,
    open_slot_index, publish_active_generation_pointer, reclaim_inactive_generation_directories,
    slot_path, sync_generation, ActiveGenerationPointer, GenerationSlot, PointerPublicationOutcome,
    INDEX_GENERATIONS_DIRECTORY,
};
#[cfg(test)]
pub(crate) use generation::{ReclamationStage, ReclamationTestHookGuard};
pub(crate) use manifest::{
    canonical_commit_payload, load_publication_for_metas, meta_generation, payload_generation_id,
    reconcile_commit_error, searcher_generation, write_manifest,
};
pub(crate) use republish::{
    best_effort_post_republish_cleanup, republish_current_with_publication_metadata,
    CurrentRepublishOutcome,
};
#[cfg(test)]
pub(crate) use republish::{
    republish_current_for_qualification, PointerReconciliationTestHookGuard, RepublishRecovery,
    RepublishStage, RepublishTestHookGuard,
};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use republish::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
#[cfg(test)]
pub(crate) use republish::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};
pub(crate) use retention::GENERATION_WRITER_LOCK_FILE;
pub use retention::{
    acquire_generation_retention_lease, load_generation_retention_lease,
    release_generation_retention_lease, GenerationRetentionLease,
};
pub(crate) use verification::{
    verify_complete_searcher, verify_publication_candidate, verify_searcher,
    verify_searcher_structure,
};

#[cfg(test)]
pub(crate) use ctx_history_index_generation::hashed_artifact_bytes;
#[cfg(test)]
pub(crate) use verification::{
    candidate_identity_verification_activity, candidate_lineage_verification_activity,
    candidate_projection_verification_activity, reset_verification_activity, verification_activity,
    verify_searcher_with_metrics,
};

#[cfg(test)]
pub(crate) use ctx_history_index_generation::{
    certification_file_for_active, MAX_CERTIFICATION_BYTES, MAX_CERTIFIED_ARTIFACTS,
};
