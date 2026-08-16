use tantivy::Index;

use crate::Result;

use crate::CandidateGeneration;

pub struct CandidateActivationFence {
    authentication: CandidateAuthentication,
}

pub struct RepublishCandidate {
    directory_name: String,
    index: Index,
    activation_fence: CandidateActivationFence,
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

    pub fn discard(self) {
        let Self {
            index,
            activation_fence,
            ..
        } = self;
        drop(index);
        activation_fence.discard();
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
