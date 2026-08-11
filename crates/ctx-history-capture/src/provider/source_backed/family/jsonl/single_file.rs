use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{CaptureProvider, SourceKey, TypedKey};

use super::{JsonlFamilyInventory, JsonlFamilyLeaf};
use crate::{common::io::ProviderSourceRoot, CaptureError, Result};

/// Binds one exact routed file to the shared JSONL family without parsing it.
pub(crate) fn jsonl_single_file_inventory(
    provider: CaptureProvider,
    route_path: &Path,
    source: SourceKey,
    path_kind: &'static str,
) -> Result<JsonlFamilyInventory> {
    let parent = route_path
        .parent()
        .ok_or_else(|| CaptureError::InvalidPayload(format!("{path_kind} path has no parent")))?;
    let authority_path = route_path
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| CaptureError::InvalidPayload(format!("{path_kind} path has no filename")))?;
    let opened = (|| -> Result<_> {
        let authority = Arc::new(ProviderSourceRoot::open(parent)?);
        let opened = authority.open_file(&authority_path)?;
        Ok((authority, opened))
    })();
    let (authority, opened) = match opened {
        Ok(opened) => opened,
        Err(error) if jsonl_error_is_not_found(&error) => {
            return JsonlFamilyInventory::missing(provider, route_path);
        }
        Err(error) => return Err(error),
    };
    let binding = TypedKey::bytes(source.exact_descriptor_digest().to_vec())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let leaf = JsonlFamilyLeaf::bind_opened(
        source,
        route_path.to_owned(),
        Arc::clone(&authority),
        authority_path,
        binding,
        &opened,
    )?;
    authority.revalidate()?;
    JsonlFamilyInventory::present(provider, route_path, authority, vec![leaf])
}

fn jsonl_error_is_not_found(error: &CaptureError) -> bool {
    match error {
        CaptureError::Io(error) => error.kind() == std::io::ErrorKind::NotFound,
        CaptureError::SystemIo { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}
