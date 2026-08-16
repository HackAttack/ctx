use std::{ffi::OsStr, path::Path};

use ctx_history_capture_model::{
    exact_bounded_string_alias, raw_object_keys_are_unique, ExactJsonStringAlias,
};
use ctx_history_provider_runtime::{
    source_io::{OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory},
    Result,
};
use sha2::{Digest, Sha256};

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

const TRAE_WORKSPACE_ALIASES: &[&str] = &["folder", "workspace", "path"];

pub(super) struct TraeWorkspaceFolderAuthority {
    literal: Option<String>,
    source: Option<OpenedProviderSourceFile>,
}

impl TraeWorkspaceFolderAuthority {
    pub(super) fn observe(parent: &ProviderSourceDirectory) -> Self {
        let Ok(OpenedProviderSourcePath::File(source)) =
            parent.open_child(OsStr::new("workspace.json"))
        else {
            return Self::unavailable();
        };
        let Ok(bytes) = source.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES) else {
            return Self::unavailable_with_source(source);
        };
        if !raw_object_keys_are_unique(&bytes) {
            return Self::unavailable_with_source(source);
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Self::unavailable_with_source(source);
        };
        let Some(object) = value.as_object() else {
            return Self::unavailable_with_source(source);
        };
        let ExactJsonStringAlias::Exact(literal) = exact_bounded_string_alias(
            object,
            TRAE_WORKSPACE_ALIASES,
            MAX_PROVIDER_JSONL_LINE_BYTES,
        ) else {
            return Self::unavailable_with_source(source);
        };
        if literal.trim().is_empty() {
            return Self::unavailable_with_source(source);
        }
        Self {
            literal: Some(literal.to_owned()),
            source: Some(source),
        }
    }

    fn unavailable() -> Self {
        Self {
            literal: None,
            source: None,
        }
    }

    fn unavailable_with_source(source: OpenedProviderSourceFile) -> Self {
        Self {
            literal: None,
            source: Some(source),
        }
    }

    pub(super) fn literal(&self) -> Option<&str> {
        self.literal.as_deref()
    }

    pub(super) fn projection_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-trae-workspace-folder-v1\0");
        match self.literal() {
            Some(literal) => {
                digest.update([1]);
                digest.update((literal.len() as u64).to_be_bytes());
                digest.update(literal.as_bytes());
            }
            None => digest.update([0]),
        }
        digest.finalize().into()
    }

    pub(super) fn certified_bytes(&self) -> u64 {
        self.literal()
            .and_then(|literal| u64::try_from(literal.len()).ok())
            .unwrap_or(0)
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        if let Some(source) = &self.source {
            source.revalidate()?;
        }
        Ok(())
    }
}

pub(super) fn trae_workspace_id(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("state-vscdb")
        .to_owned()
}
