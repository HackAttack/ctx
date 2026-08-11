use std::{
    fs::{File, Metadata},
    path::Path,
};

use ctx_history_source_io::retained_jsonl_file_identity_v1;

use super::JsonlFileObservation;
use crate::Result;

pub(super) fn observe_metadata(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<JsonlFileObservation> {
    let identity = retained_jsonl_file_identity_v1(path, file, metadata)?;
    Ok(JsonlFileObservation::new(
        metadata.len(),
        metadata.modified()?,
        metadata.permissions().readonly(),
        identity.map(|identity| identity.stable()),
        identity.map(|identity| identity.change()),
    ))
}
