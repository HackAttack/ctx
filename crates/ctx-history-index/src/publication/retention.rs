use std::path::Path;

use crate::Result;

pub use ctx_history_index_generation::GenerationRetentionLease;
pub(crate) use ctx_history_index_generation::GENERATION_WRITER_LOCK_FILE;

pub fn acquire_generation_retention_lease(
    root: impl AsRef<Path>,
    generation_id: &str,
    owner_kind: &str,
    owner_id: &str,
) -> Result<GenerationRetentionLease> {
    Ok(
        ctx_history_index_generation::acquire_generation_retention_lease(
            root,
            generation_id,
            owner_kind,
            owner_id,
        )?,
    )
}

pub fn load_generation_retention_lease(
    root: impl AsRef<Path>,
) -> Result<Option<GenerationRetentionLease>> {
    Ok(ctx_history_index_generation::load_generation_retention_lease(root)?)
}

pub fn release_generation_retention_lease(
    root: impl AsRef<Path>,
    expected: &GenerationRetentionLease,
) -> Result<bool> {
    Ok(ctx_history_index_generation::release_generation_retention_lease(root, expected)?)
}
