use super::*;
use ctx_history_core::SessionRelationshipKind;
use ctx_history_source_io::SourceIoError;

pub(super) fn trim_jsonl_terminator(mut record: &[u8]) -> &[u8] {
    if record.last() == Some(&b'\r') {
        record = &record[..record.len() - 1];
    }
    record
}

pub(super) fn observed_opened_file(
    source: &CodexCatalogSource,
    opened: &OpenedProviderSourceFile,
) -> Result<CodexFileObservation> {
    let current = opened_file_observation(&source.source_path, opened.file())?;
    opened.revalidate_same_object()?;
    if !source
        .catalog_observation
        .admits_append_only_growth(&current)
    {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog observation changed before NativePath admission".to_owned(),
        ));
    }
    // The strong ordinary-file observation already binds an unchanged file's
    // identity and change token. Keep exact no-op admission metadata-only;
    // growth still proves the complete frozen prefix below.
    if current == source.catalog_observation {
        return Ok(source.catalog_observation.clone());
    }
    let expected_prefix = source
        .catalog_prefix_sha256
        .ok_or(CaptureError::SourceChangedDuringCapture)?;
    if opened_file_prefix_sha256(opened.file(), source.catalog_observation.len)? != expected_prefix
    {
        return Err(source_changed_during_scan());
    }
    opened.revalidate_same_object()?;
    // Discovery admitted this ordinary-file identity and froze this refresh's
    // EOF. Growth after that observation is deferred to the next refresh;
    // broadening the boundary here would give one source two authorities.
    Ok(source.catalog_observation.clone())
}

pub(crate) fn opened_file_prefix_sha256(file: &File, len: u64) -> Result<[u8; 32]> {
    ctx_history_source_io::opened_file_prefix_sha256(file, len)
        .map_err(map_ordinary_file_observation_error)
}

pub(crate) fn reopen_codex_source_capability(
    source: &CodexCatalogSource,
) -> Result<Arc<OpenedProviderSourceFile>> {
    match (
        source.authority_root.as_ref(),
        source.authority_relative_path.as_ref(),
    ) {
        (Some(root), Some(relative_path)) => Ok(Arc::new(root.open_file(relative_path)?)),
        (None, None) => {
            let authority_path = std::path::absolute(&source.source_path)?;
            Ok(Arc::new(open_provider_source_file(&authority_path)?))
        }
        _ => Err(CaptureError::SystemInvariant(
            "Codex source route authority is incomplete",
        )),
    }
}

pub(crate) fn revalidate_codex_catalog_source_capability(
    source: &CodexCatalogSource,
    opened: &OpenedProviderSourceFile,
) -> Result<()> {
    match observed_opened_file(source, opened) {
        Ok(_) => Ok(()),
        // This proof is used only after generation discovery has admitted the
        // catalog observation. A later ordinary-file mismatch is a retryable
        // replacement race, not a permanently invalid transcript.
        Err(CaptureError::InvalidPayload(_)) => Err(CaptureError::SourceChangedDuringCapture),
        Err(error) => Err(error),
    }
}

pub(crate) fn opened_file_observation(path: &Path, file: &File) -> Result<CodexFileObservation> {
    let observation = ctx_history_source_io::observe_opened_ordinary_file_v2(path, file)
        .map_err(map_ordinary_file_observation_error)?;
    Ok(CodexFileObservation::from_parts(
        observation.len(),
        observation.modified_at(),
        observation.stable_token(),
        observation.change_token(),
    ))
}

fn map_ordinary_file_observation_error(error: SourceIoError) -> CaptureError {
    match error {
        SourceIoError::SourceChangedDuringCapture => source_changed_during_scan(),
        error => error.into(),
    }
}

pub(super) fn validate_catalog_owner(
    source: &CodexCatalogSource,
    mut scanned_owner: CodexSessionRow,
) -> Result<CodexSessionRow> {
    let catalog_owner = source.catalog_native_session_id.as_deref();
    let root_native_session_id = match scanned_owner.session_relationship {
        SessionRelationshipKind::Root if scanned_owner.parent_native_session_id.is_none() => {
            scanned_owner.native_session_id.clone()
        }
        SessionRelationshipKind::RelatedUnknown | SessionRelationshipKind::Root => {
            return Err(CaptureError::InvalidPayload(
                "Codex normalized catalog owner changed before NativePath admission".to_owned(),
            ));
        }
        _ => scanned_owner
            .parent_native_session_id
            .clone()
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Codex normalized catalog owner changed before NativePath admission".to_owned(),
                )
            })?,
    };
    if catalog_owner != Some(scanned_owner.native_session_id.as_str()) {
        return Err(CaptureError::InvalidPayload(
            "Codex normalized catalog owner changed before NativePath admission".to_owned(),
        ));
    }
    scanned_owner.root_native_session_id = Some(root_native_session_id);
    Ok(scanned_owner)
}

pub(super) fn source_changed_during_scan() -> CaptureError {
    CaptureError::InvalidPayload("Codex source changed while NativePath was reading it".to_owned())
}
