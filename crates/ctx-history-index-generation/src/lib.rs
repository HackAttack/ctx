//! Durable control and physical storage for immutable lexical generations.
//!
//! Lexical query, lineage policy, and writer orchestration remain in
//! `ctx-history-index`; this crate owns only their non-lineage persisted
//! generation substrate.

mod certification;
mod clone;
mod durable_directory;
mod error;
mod generation;
mod identity;
mod lock;
mod manifest;
mod physical;
mod retention;

#[cfg(any(test, feature = "test-support"))]
pub use certification::{
    certification_file_for_active, MAX_CERTIFICATION_BYTES, MAX_CERTIFIED_ARTIFACTS,
};
pub use certification::{
    certify_activated_generation, reclaim_unreferenced_certifications,
    scrub_and_certify_physical_integrity, verify_certified_physical_integrity,
    verify_or_certify_physical_integrity, CertifiedPhysicalIntegrity,
};
#[cfg(any(test, feature = "test-support"))]
pub use clone::{
    candidate_clone_metrics, reset_candidate_clone_metrics, CandidateCloneMetrics,
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};
pub use clone::{
    create_authenticated_candidate_generation, create_authenticated_republish_candidate,
    RepublishCandidate,
};
#[cfg(all(
    any(test, feature = "test-support"),
    any(target_os = "linux", target_os = "macos")
))]
pub use clone::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
pub use durable_directory::{
    durable_atomic_replace_file, reclaim_abandoned_atomic_writes, DurableAtomicWriteOutcome,
    DurableMmapDirectory,
};
#[cfg(any(test, feature = "test-support"))]
pub use durable_directory::{AtomicWriteStage, AtomicWriteTestHookGuard};
pub use error::{GenerationError, Result};
pub use generation::{
    create_candidate_generation, lexical_index_settings, load_active_generation_pointer,
    open_slot_index, publish_active_generation_pointer, reclaim_inactive_generation_directories,
    slot_path, sync_directory, sync_generation, ActiveGenerationPointer, CandidateGeneration,
    GenerationSlot, PointerPublicationOutcome,
};
#[cfg(any(test, feature = "test-support"))]
pub use generation::{ReclamationStage, ReclamationTestHookGuard};
pub use identity::{hex, is_generation_id, sha256_hex};
pub use lock::acquire_generation_writer_lock_with_retry;
pub use manifest::{load_manifest_bytes, reclaim_unreferenced_manifests, write_manifest_bytes};
pub use physical::{
    active_index_files, physical_integrity_audit, physical_integrity_audit_with_candidate_proof,
    physical_integrity_digest, prime_candidate_physical_proof, verify_physical_integrity,
    CandidatePhysicalProof, PhysicalIntegrityAudit,
};
#[cfg(any(test, feature = "test-support"))]
pub use physical::{checksum_walks, hashed_artifact_bytes, reset_physical_verification_activity};
pub use retention::{
    acquire_generation_retention_lease, load_generation_retention_lease,
    release_generation_retention_lease, GenerationRetentionLease,
};

pub const MANIFEST_DIRECTORY: &str = "ctx-generations";
pub const INDEX_GENERATIONS_DIRECTORY: &str = "index-generations";
pub const ACTIVE_GENERATION_POINTER_FILE: &str = "active-generation.json";
pub const GENERATION_WRITER_LOCK_FILE: &str = ".ctx-generation-writer.lock";

pub fn manifest_path(root: &std::path::Path, generation_id: &str) -> std::path::PathBuf {
    root.join(MANIFEST_DIRECTORY)
        .join(format!("{generation_id}.json"))
}
