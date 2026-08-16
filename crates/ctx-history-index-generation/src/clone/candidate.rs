use std::path::PathBuf;

use tantivy::Index;

use crate::{
    refresh_certification_after_managed_reclamation, ActiveGenerationPointer,
    CertifiedPhysicalIntegrity, GenerationError as IndexError, GenerationOwnershipFence,
    GenerationSlot, Result,
};

use crate::CandidateGeneration;

pub struct CandidateActivationFence {
    authentication: CandidateAuthentication,
}

/// Keeps readers out while a Linux hardlink-backed candidate may change the
/// active generation's authenticated link topology.
#[doc(hidden)]
pub struct CandidateOwnershipFence {
    root: PathBuf,
    pointer: ActiveGenerationPointer,
    slot: GenerationSlot,
    index: Index,
    certified_after_clone: CertifiedPhysicalIntegrity,
    fence: Option<GenerationOwnershipFence>,
}

impl CandidateOwnershipFence {
    #[cfg(target_os = "linux")]
    pub(super) fn new(
        root: PathBuf,
        pointer: ActiveGenerationPointer,
        slot: GenerationSlot,
        index: Index,
        certified_after_clone: CertifiedPhysicalIntegrity,
        fence: GenerationOwnershipFence,
    ) -> Self {
        Self {
            root,
            pointer,
            slot,
            index,
            certified_after_clone,
            fence: Some(fence),
        }
    }

    fn refresh_base_certification(&self) -> Result<()> {
        refresh_certification_after_managed_reclamation(
            &self.root,
            &self.pointer,
            &self.slot,
            &self.index,
            &self.certified_after_clone,
        )
    }

    fn ensure_ownership_fence(&mut self) -> Result<()> {
        if self.fence.is_none() {
            let fence = crate::acquire_generation_ownership_fence(&self.root)?;
            self.refresh_base_certification()?;
            self.fence = Some(fence);
        }
        Ok(())
    }

    /// A republish candidate never mutates inherited artifacts after cloning,
    /// so readers may resume against the newly certified stable alias set.
    #[cfg(target_os = "linux")]
    pub(super) fn release_stable_alias_fence(mut self) -> Self {
        drop(self.fence.take());
        self
    }

    /// Refreshes the base after all candidate mutations, then transfers the
    /// ownership fence to the terminal pointer publication.
    pub fn into_publication_fence(mut self) -> Result<GenerationOwnershipFence> {
        self.ensure_ownership_fence()?;
        self.refresh_base_certification()?;
        self.fence
            .take()
            .ok_or(IndexError::ConcurrentGenerationChange)
    }

    fn publication_fence(&mut self) -> Result<&GenerationOwnershipFence> {
        self.ensure_ownership_fence()?;
        self.refresh_base_certification()?;
        self.fence
            .as_ref()
            .ok_or(IndexError::ConcurrentGenerationChange)
    }
}

impl Drop for CandidateOwnershipFence {
    fn drop(&mut self) {
        if self.fence.is_some() {
            let _ = self.refresh_base_certification();
        }
    }
}

pub struct RepublishCandidate {
    directory_name: String,
    index: Index,
    activation_fence: CandidateActivationFence,
    ownership_fence: Option<CandidateOwnershipFence>,
}

pub(super) enum CandidateAuthentication {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    DescriptorClone(super::unix::CandidateGuard),
    #[cfg(any(
        test,
        feature = "test-support",
        target_os = "windows",
        target_os = "freebsd"
    ))]
    Portable(super::portable::CandidateGuard),
}

impl RepublishCandidate {
    pub(super) fn new(candidate: CandidateGeneration) -> Self {
        Self {
            directory_name: candidate.directory_name,
            index: candidate.index,
            activation_fence: candidate.activation_fence,
            ownership_fence: candidate.ownership_fence,
        }
    }

    pub fn directory_name(&self) -> &str {
        &self.directory_name
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn validate_binding(&self) -> Result<()> {
        self.activation_fence.validate_binding()
    }

    #[doc(hidden)]
    pub fn publication_fence(&mut self) -> Result<Option<&GenerationOwnershipFence>> {
        self.ownership_fence
            .as_mut()
            .map(CandidateOwnershipFence::publication_fence)
            .transpose()
    }

    pub fn discard(self) -> Result<()> {
        let Self {
            index,
            activation_fence,
            mut ownership_fence,
            ..
        } = self;
        if let Some(fence) = ownership_fence.as_mut() {
            fence.ensure_ownership_fence()?;
        }
        drop(index);
        activation_fence.discard();
        if let Some(fence) = ownership_fence {
            drop(fence.into_publication_fence()?);
        }
        Ok(())
    }
}

impl CandidateActivationFence {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn descriptor_clone(guard: super::unix::CandidateGuard) -> Self {
        Self {
            authentication: CandidateAuthentication::DescriptorClone(guard),
        }
    }

    #[cfg(any(
        test,
        feature = "test-support",
        target_os = "windows",
        target_os = "freebsd"
    ))]
    pub(super) fn portable(guard: super::portable::CandidateGuard) -> Self {
        Self {
            authentication: CandidateAuthentication::Portable(guard),
        }
    }

    pub fn validate_binding(&self) -> Result<()> {
        match &self.authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.validate_binding(),
            #[cfg(any(
                test,
                feature = "test-support",
                target_os = "windows",
                target_os = "freebsd"
            ))]
            CandidateAuthentication::Portable(guard) => guard.validate_binding(),
        }
    }

    pub fn discard(self) {
        match self.authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.discard(),
            #[cfg(any(
                test,
                feature = "test-support",
                target_os = "windows",
                target_os = "freebsd"
            ))]
            CandidateAuthentication::Portable(guard) => guard.discard(),
        }
    }
}
