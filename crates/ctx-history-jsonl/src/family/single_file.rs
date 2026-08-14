use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{CaptureProvider, SourceKey, TypedKey};

use super::{JsonlFamilyError, JsonlResult, ProviderSourceRoot};
use super::{JsonlFamilyInventory, JsonlFamilyLeaf};

/// Binds one exact routed file to the shared JSONL family without parsing it.
pub fn jsonl_single_file_inventory<E: JsonlFamilyError>(
    provider: CaptureProvider,
    route_path: &Path,
    source: SourceKey,
    path_kind: &'static str,
) -> JsonlResult<JsonlFamilyInventory<E>, E> {
    let parent = route_path
        .parent()
        .ok_or_else(|| E::invalid_payload(format!("{path_kind} path has no parent")))?;
    let authority_path = route_path
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| E::invalid_payload(format!("{path_kind} path has no filename")))?;
    let opened = (|| -> JsonlResult<_, E> {
        let authority = Arc::new(ProviderSourceRoot::<E>::open(parent)?);
        let opened = authority.open_file(&authority_path)?;
        Ok((authority, opened))
    })();
    let (authority, opened) = match opened {
        Ok(opened) => opened,
        Err(error) if error.is_not_found() => {
            return JsonlFamilyInventory::missing(provider, route_path);
        }
        Err(error) => return Err(error),
    };
    let binding = TypedKey::bytes(source.exact_descriptor_digest().to_vec())
        .map_err(|error| E::invalid_payload(error.to_string()))?;
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
