use super::*;

pub(crate) fn seed_filter_unaware_manifest(
    root: &Path,
    contract: FlatModelContract,
) -> FlatResult<()> {
    const FILTER_UNAWARE_FLAT_SCHEMA_VERSION: u32 = 3;

    let selected = match select_manifest_any(root)? {
        Some(selected) => selected,
        None => {
            let store = FlatSegmentStore::open(root, contract)?;
            store.publish_replacement_event_chunks(&[], &[Uuid::from_u128(1)])?;
            drop(store);
            select_manifest_any(root)?.ok_or_else(|| {
                FlatStoreError::Corrupt("legacy test manifest publication is missing".to_owned())
            })?
        }
    };
    let mut legacy = selected.envelope;
    legacy.manifest.schema_version = FILTER_UNAWARE_FLAT_SCHEMA_VERSION;
    fs::write(
        &selected.path,
        serde_json::to_vec(&legacy).map_err(FlatStoreError::Serialize)?,
    )
    .map_err(|source| io_error("write legacy test manifest", &selected.path, source))
}
