use super::super::rows::source_backed_tool_context;
use super::*;
use crate::provider::source_backed::family::jsonl::{
    retained_file_identity, JsonlFileIdentityPolicy,
};
use ctx_history_core::SessionRelationshipKind;

pub(super) fn trim_jsonl_terminator(mut record: &[u8]) -> &[u8] {
    if record.last() == Some(&b'\r') {
        record = &record[..record.len() - 1];
    }
    record
}

pub(super) fn decode_pending_tool_authority(
    record: &[u8],
    authority: &CodexPendingToolAuthority,
    owner: &CodexSessionRow,
    local_turn_started: bool,
) -> Result<(String, CodexToolCallContext)> {
    match authority.invocation_origin() {
        CodexInvocationOriginV0::UniqueToSession
            if owner.session_relationship == SessionRelationshipKind::RelatedUnknown =>
        {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority claims unique origin for unknown lineage",
            ));
        }
        CodexInvocationOriginV0::UniqueToSession
            if matches!(
                owner.session_relationship,
                SessionRelationshipKind::Forked | SessionRelationshipKind::ResumedFrom
            ) && !local_turn_started =>
        {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority claims unique origin before a local turn",
            ));
        }
        CodexInvocationOriginV0::CopiedFromAncestor {
            ancestor_native_session_id,
        } if !matches!(
            owner.session_relationship,
            SessionRelationshipKind::Forked | SessionRelationshipKind::ResumedFrom
        ) || owner.parent_native_session_id.as_deref()
            != Some(ancestor_native_session_id.as_str()) =>
        {
            return Err(invalid_checkpoint_proof(
                "pending tool-call copied origin does not match source ownership",
            ));
        }
        CodexInvocationOriginV0::UniqueToSession
        | CodexInvocationOriginV0::CopiedFromAncestor { .. }
        | CodexInvocationOriginV0::Unproven => {}
    }
    // The surrounding checkpoint walk has already matched this authority to
    // an exact JSONL boundary. The current scanner scratch omits the delimiter;
    // the pending-authority scratch includes it.
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    let record = trim_jsonl_terminator(record);
    let probe = classify_codex_record(record).map_err(|_| {
        invalid_checkpoint_proof("pending tool-call authority is not valid Codex JSON")
    })?;
    if probe.lineage_malformed() {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority has malformed lineage fields",
        ));
    }
    let CodexRecordClass::Retained(kind @ super::super::record::CodexRetainedKind::ToolCall) =
        probe.class
    else {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority does not identify a tool call",
        ));
    };
    let retained = parse_decoded_record(record, owner)
        .ok_or_else(|| invalid_checkpoint_proof("pending tool-call authority cannot be decoded"))?;
    let (call_id, mut context) = match source_backed_tool_context(kind, &retained) {
        Ok(Some(tool_context)) => tool_context,
        Ok(None) => {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority has no correlation identity",
            ));
        }
        Err(
            CodexRetainedNonMaterialized::ValidUnmaterializable
            | CodexRetainedNonMaterialized::Malformed,
        ) => {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority cannot be projected",
            ));
        }
    };
    if !authority.matches_call_id(&call_id) {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority correlation does not match checkpoint state",
        ));
    }
    if let [evidence] =
        crate::provider::codex::repository::repository_tool_evidence(&retained.payload).as_slice()
    {
        // Fresh source-backed projection redacts provider-native arguments
        // from display/Core text. Append-proof reconstruction must recover
        // that same bounded context, not revive the legacy preview.
        context.command_preview = None;
        context.arguments_preview = None;
        context.tool_name.clone_from(&evidence.tool_name);
        context.session_cwd = owner.cwd.clone();
        context.exact_command.clone_from(&evidence.command);
        context.command_too_large = evidence.command_too_large;
        context
            .declared_workdir
            .clone_from(&evidence.declared_workdir);
        context
            .continuation_cell_id
            .clone_from(&evidence.continuation_cell_id);
        if context.exact_command.is_some() || context.command_too_large {
            context.origin_call_id = Some(call_id.clone());
            context.origin_event_sequence = Some(authority.raw_ordinal);
            context.origin_occurred_at_unix_ms = Some(retained.occurred_at.timestamp_millis());
        }
    }
    context.continuation_call_id_sha256 = authority.continuation_call_id_sha256().to_vec();
    context.continuation_capacity_exceeded = authority.continuation_capacity_exceeded();
    context.correlation_ambiguous = authority.correlation_ambiguous();
    context
        .invocation_origin
        .clone_from(authority.invocation_origin());
    Ok((call_id, bound_tool_context(context)))
}

pub(super) fn restore_pending_continuations(
    pending_tool_contexts: &mut BTreeMap<String, CodexToolCallContext>,
    pending_tool_authorities: &BTreeMap<String, CodexPendingToolAuthority>,
) -> Result<BTreeMap<String, String>> {
    let mut pending_continuations = BTreeMap::new();
    for (call_id, authority) in pending_tool_authorities {
        if let Some(cell_id) = authority.continuation_cell_id() {
            if authority.continuation_conflicted() {
                if pending_continuations
                    .insert(cell_id.to_owned(), String::new())
                    .is_some()
                {
                    return Err(invalid_checkpoint_proof(
                        "pending conflicted continuation cell is duplicated",
                    ));
                }
                continue;
            }
            let Some(origin) = pending_tool_contexts.get(call_id) else {
                return Err(invalid_checkpoint_proof(
                    "pending continuation origin context is unavailable",
                ));
            };
            if (origin.exact_command.is_none() && !origin.command_too_large)
                || origin.continuation_cell_id.is_some()
            {
                return Err(invalid_checkpoint_proof(
                    "pending continuation authority is not an exact origin command",
                ));
            }
            if pending_continuations
                .insert(cell_id.to_owned(), call_id.clone())
                .is_some()
            {
                return Err(invalid_checkpoint_proof(
                    "pending continuation cell is assigned more than once",
                ));
            }
        }
    }
    let wait_calls = pending_tool_contexts
        .iter()
        .filter_map(|(call_id, context)| {
            context
                .continuation_cell_id
                .as_ref()
                .map(|cell_id| (call_id.clone(), cell_id.clone()))
        })
        .collect::<Vec<_>>();
    for (call_id, cell_id) in wait_calls {
        let Some(origin_call_id) = pending_continuations.get(&cell_id) else {
            continue;
        };
        if origin_call_id.is_empty() {
            continue;
        }
        let origin = pending_tool_contexts
            .get(origin_call_id)
            .cloned()
            .ok_or_else(|| {
                invalid_checkpoint_proof("pending continuation origin is unavailable")
            })?;
        let context = pending_tool_contexts.get_mut(&call_id).ok_or_else(|| {
            invalid_checkpoint_proof("pending continuation wait context is unavailable")
        })?;
        context.exact_command = origin.exact_command;
        context.command_too_large = origin.command_too_large;
        context.session_cwd = origin.session_cwd;
        context.declared_workdir = origin.declared_workdir;
        context.origin_call_id = Some(origin_call_id.clone());
        context.origin_event_sequence = origin.origin_event_sequence;
        context.origin_occurred_at_unix_ms = origin.origin_occurred_at_unix_ms;
        context.continuation_call_id_sha256 = origin.continuation_call_id_sha256;
        context.continuation_capacity_exceeded = origin.continuation_capacity_exceeded;
        context.correlation_ambiguous = origin.correlation_ambiguous;
        context.invocation_origin = origin.invocation_origin;
    }
    Ok(pending_continuations)
}

pub(super) fn invalid_checkpoint_proof(reason: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid Codex semantic checkpoint: {reason}"))
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
    revalidate_opened_prefix(
        opened.file(),
        source.catalog_observation.len,
        expected_prefix,
    )?;
    opened.revalidate_same_object()?;
    // Discovery admitted this ordinary-file identity and froze this refresh's
    // EOF. Growth after that observation is deferred to the next refresh;
    // broadening the boundary here would give one source two authorities.
    Ok(source.catalog_observation.clone())
}

pub(super) fn revalidate_opened_prefix(
    file: &File,
    len: u64,
    expected_sha256: [u8; 32],
) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut reader = file.try_clone()?;
    hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    if <[u8; 32]>::from(hasher.finalize()) != expected_sha256 {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

pub(crate) fn opened_file_prefix_sha256(file: &File, len: u64) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut reader = file.try_clone()?;
    hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    Ok(hasher.finalize().into())
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
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(source_changed_during_scan());
    }
    let platform_before =
        retained_file_identity(path, file, &metadata, JsonlFileIdentityPolicy::OrdinaryV2)?;
    let content_fingerprint = if platform_before.is_some() {
        None
    } else {
        Some(opened_file_content_fingerprint(file, &metadata)?)
    };
    let current = file.metadata()?;
    let platform_after =
        retained_file_identity(path, file, &current, JsonlFileIdentityPolicy::OrdinaryV2)?;
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(source_changed_during_scan());
    }
    Ok(CodexFileObservation::from_parts(
        metadata.len(),
        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        platform_before.map(|tokens| tokens.stable()),
        combine_opened_file_token(
            platform_before.map(|tokens| tokens.change()),
            content_fingerprint,
        ),
    ))
}

fn combine_opened_file_token(
    platform_token: Option<[u8; 32]>,
    content_fingerprint: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    if let Some(platform_token) = platform_token {
        hasher.update(b"platform\0");
        hasher.update(platform_token);
    } else {
        hasher.update(b"portable\0");
        match content_fingerprint {
            Some(content_fingerprint) => hasher.update(content_fingerprint),
            None => hasher.update(b"missing-content-fingerprint\0"),
        }
    }
    hasher.finalize().into()
}

fn opened_file_content_fingerprint(file: &File, metadata: &std::fs::Metadata) -> Result<[u8; 32]> {
    let len = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    hasher.update(len.to_le_bytes());
    let mut reader = file.try_clone()?;
    let original_position = reader.stream_position()?;
    if len <= ORDINARY_FILE_FULL_FINGERPRINT_MAX_BYTES {
        hasher.update(b"full\0");
        hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    } else {
        hasher.update(b"sparse\0");
        for offset in opened_file_sparse_sample_offsets(len) {
            let sample_len = ORDINARY_FILE_SPARSE_SAMPLE_BYTES.min(len.saturating_sub(offset));
            hasher.update(offset.to_le_bytes());
            hasher.update(sample_len.to_le_bytes());
            hash_opened_file_range(&mut reader, offset, sample_len, &mut hasher)?;
        }
    }
    reader.seek(SeekFrom::Start(original_position))?;
    Ok(hasher.finalize().into())
}

fn opened_file_sparse_sample_offsets(len: u64) -> std::collections::BTreeSet<u64> {
    let last = len.saturating_sub(ORDINARY_FILE_SPARSE_SAMPLE_BYTES);
    [0, len / 4, len / 2, len.saturating_mul(3) / 4, last]
        .into_iter()
        .map(|offset| offset.min(last))
        .collect()
}

fn hash_opened_file_range(
    file: &mut File,
    offset: u64,
    len: u64,
    hasher: &mut Sha256,
) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut buffer = [0_u8; 8 * 1024];
    while remaining > 0 {
        let take = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(source_changed_during_scan());
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok(())
}

pub(super) fn validate_catalog_owner(
    source: &CodexCatalogSource,
    mut scanned_owner: CodexSessionRow,
) -> Result<CodexSessionRow> {
    let catalog_owner = source.catalog_native_session_id.as_deref();
    let catalog_root = source.catalog_root_native_session_id.as_deref();
    if catalog_owner != Some(scanned_owner.native_session_id.as_str())
        || source.catalog_parent_native_session_id != scanned_owner.parent_native_session_id
        || source.catalog_session_relationship != scanned_owner.session_relationship
        || source.catalog_advisory_session_id != scanned_owner.advisory_session_id
        || catalog_root.is_none()
        || scanned_owner
            .root_native_session_id
            .as_deref()
            .is_some_and(|scanned_root| Some(scanned_root) != catalog_root)
    {
        return Err(CaptureError::InvalidPayload(
            "Codex normalized catalog owner changed before NativePath admission".to_owned(),
        ));
    }
    scanned_owner.root_native_session_id = catalog_root.map(str::to_owned);
    Ok(scanned_owner)
}

pub(super) fn validate_checkpoint_catalog_owner(
    source: &CodexCatalogSource,
    scanned_owner: CodexSessionRow,
) -> Result<CodexSessionRow> {
    if scanned_owner.root_native_session_id.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Codex checkpoint owner is not normalized".to_owned(),
        ));
    }
    validate_catalog_owner(source, scanned_owner)
}

pub(super) fn source_changed_during_scan() -> CaptureError {
    CaptureError::InvalidPayload("Codex source changed while NativePath was reading it".to_owned())
}
