use tantivy::Index;

use crate::Result;

use crate::CandidateGeneration;

pub struct RepublishCandidate {
    directory_name: String,
    index: Index,
    authentication: CandidateAuthentication,
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
    pub(super) fn new(
        candidate: CandidateGeneration,
        authentication: CandidateAuthentication,
    ) -> Self {
        Self {
            directory_name: candidate.directory_name,
            index: candidate.index,
            authentication,
        }
    }

    pub fn directory_name(&self) -> &str {
        &self.directory_name
    }

    pub fn index(&self) -> &Index {
        &self.index
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
        let Self {
            index,
            authentication,
            ..
        } = self;
        drop(index);
        match authentication {
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
