use super::*;

/// Verifies a durably certified candidate before it is named by the active
/// pointer. The predecessor fence is not part of the certification identity.
pub fn verify_candidate_physical_integrity_read_only(
    root: &Path,
    predecessor_fence: &ActiveGenerationPointerFence,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY))?;

    predecessor_fence.validate(root)?;
    let bytes =
        read_certification(&certification_path(root, slot)).ok_or(IndexError::ChecksumMismatch)?;
    let certification = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.slot != *slot
        || !certification_digest_matches_slot(&certification)?
        || capture_single_link_control(&manifest_path(root, slot.generation_id()))?
            != certification.manifest_identity
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let retained_alias_directories = predecessor_fence
        .topology_authority()
        .into_iter()
        .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
        .chain(std::iter::once(slot))
        .map(|slot| slot.directory().to_owned())
        .collect::<HashSet<_>>();
    for expected in &certification.artifacts {
        let current = capture_artifact_with_retained_aliases(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            &retained_alias_directories,
        )?;
        if current != expected.artifact {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    predecessor_fence.validate(root)?;
    Ok(())
}

/// Durably certifies one exact, fully hashed candidate before pointer
/// publication. The sidecar is bound to its immutable slot, manifest, and
/// artifact identities rather than to the active-pointer inode.
pub fn certify_candidate_physical_integrity(
    root: &Path,
    predecessor_fence: &ActiveGenerationPointerFence,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
) -> Result<CertifiedPhysicalIntegrity> {
    install_certification(
        root,
        predecessor_fence.topology_authority(),
        Some(predecessor_fence),
        slot,
        index,
        audit,
        CertificationInstallPolicy::CANDIDATE,
    )
}

pub(super) fn certification_digest_matches_slot(
    certification: &GenerationIntegrityCertification,
) -> Result<bool> {
    Ok(
        physical_integrity_digest_from_parts(certification.artifacts.iter().map(|artifact| {
            (
                artifact.artifact.path.as_str(),
                artifact.artifact.identity.length(),
                artifact.sha256,
            )
        }))? == certification.slot.physical_integrity_digest(),
    )
}
