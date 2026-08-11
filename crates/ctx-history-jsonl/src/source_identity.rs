use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlSourceIdentity {
    provider: String,
    parser_revision: String,
    policy_revision: String,
    source_descriptor_digest: [u8; 32],
    source_path: PathBuf,
}

impl JsonlSourceIdentity {
    pub fn new(
        provider: impl Into<String>,
        parser_revision: impl Into<String>,
        policy_revision: impl Into<String>,
        source_descriptor_digest: [u8; 32],
        source_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider: provider.into(),
            parser_revision: parser_revision.into(),
            policy_revision: policy_revision.into(),
            source_descriptor_digest,
            source_path: source_path.into(),
        }
    }

    pub fn source_descriptor_digest(&self) -> &[u8; 32] {
        &self.source_descriptor_digest
    }

    pub fn source_path(&self) -> &PathBuf {
        &self.source_path
    }
}
