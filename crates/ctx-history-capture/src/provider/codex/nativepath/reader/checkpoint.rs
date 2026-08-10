use super::super::rows::{build_event_row, tool_context_from_row};
use super::*;
use crate::provider::source_backed::family::jsonl::{
    read_bounded_record as read_shared_bounded_record,
    read_bounded_record_unhashed as read_shared_bounded_record_unhashed, retained_file_identity,
    JsonlBoundedRecordRead as BoundedRecordRead, JsonlFileIdentityPolicy, JsonlRecordFraming,
};

pub(super) fn read_bounded_record(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut Sha256,
    complete_hasher: &mut Sha256,
    maximum_bytes: u64,
) -> Result<Option<BoundedRecordRead>> {
    read_shared_bounded_record(
        reader,
        storage,
        full_hasher,
        complete_hasher,
        maximum_bytes,
        JsonlRecordFraming::terminal_nul_padded(crate::MAX_PROVIDER_JSONL_LINE_BYTES),
        source_changed_during_scan,
    )
}

pub(super) fn read_bounded_record_unhashed(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    maximum_bytes: u64,
) -> Result<Option<BoundedRecordRead>> {
    read_shared_bounded_record_unhashed(
        reader,
        storage,
        maximum_bytes,
        JsonlRecordFraming::terminal_nul_padded(crate::MAX_PROVIDER_JSONL_LINE_BYTES),
        source_changed_during_scan,
    )
}

#[cfg(test)]
mod bounded_record_tests {
    use super::*;

    fn assert_digest_policies_match(contents: &[u8], frozen_len: u64) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bounded-record.jsonl");
        std::fs::write(&path, contents).unwrap();
        let mut hashed_reader = BufReader::with_capacity(8 * 1024, File::open(&path).unwrap());
        let mut unhashed_reader = BufReader::with_capacity(64 * 1024, File::open(&path).unwrap());
        let mut hashed_storage = Vec::new();
        let mut unhashed_storage = Vec::new();
        let mut full_hasher = Sha256::new();
        let mut complete_hasher = Sha256::new();
        let mut offset = 0_u64;
        let mut complete_end = 0_u64;

        while offset < frozen_len {
            let hashed = read_bounded_record(
                &mut hashed_reader,
                &mut hashed_storage,
                &mut full_hasher,
                &mut complete_hasher,
                frozen_len.saturating_sub(offset),
            )
            .unwrap()
            .unwrap();
            let unhashed = read_bounded_record_unhashed(
                &mut unhashed_reader,
                &mut unhashed_storage,
                frozen_len.saturating_sub(offset),
            )
            .unwrap()
            .unwrap();

            assert_eq!(unhashed.complete, hashed.complete);
            assert_eq!(unhashed.terminal_nul_padding, hashed.terminal_nul_padding);
            assert_eq!(unhashed.oversized, hashed.oversized);
            assert_eq!(unhashed.stored_len, hashed.stored_len);
            assert_eq!(unhashed.byte_len, hashed.byte_len);
            assert_eq!(unhashed.sha256, [0; 32]);
            assert_eq!(unhashed_storage, hashed_storage);

            let record_end = offset.saturating_add(hashed.byte_len);
            if hashed.terminal_nul_padding {
                assert_eq!(hashed.sha256, [0; 32]);
            } else {
                let start = usize::try_from(offset).unwrap();
                let end = usize::try_from(record_end).unwrap();
                assert_eq!(
                    hashed.sha256,
                    <[u8; 32]>::from(Sha256::digest(&contents[start..end]))
                );
            }
            if hashed.complete {
                complete_end = record_end;
            }
            offset = record_end;
        }

        let frozen_end = usize::try_from(frozen_len).unwrap();
        assert_eq!(offset, frozen_len);
        assert_eq!(
            <[u8; 32]>::from(full_hasher.finalize()),
            <[u8; 32]>::from(Sha256::digest(&contents[..frozen_end]))
        );
        assert_eq!(
            <[u8; 32]>::from(complete_hasher.finalize()),
            <[u8; 32]>::from(Sha256::digest(
                &contents[..usize::try_from(complete_end).unwrap()]
            ))
        );
    }

    #[test]
    fn digest_policies_preserve_framing_bounds_and_incomplete_tails() {
        let mut records = b"alpha\r\n".to_vec();
        records.resize(records.len() + MAX_CODEX_RECORD_BYTES + 17, b'x');
        records.push(b'\n');
        records.extend_from_slice(b"incomplete tail");
        assert_digest_policies_match(&records, records.len() as u64);

        let terminal_nul = vec![0; 64 * 1024 + 17];
        assert_digest_policies_match(&terminal_nul, terminal_nul.len() as u64);

        let frozen = b"first\nsecond\nnot admitted";
        assert_digest_policies_match(frozen, b"first\nsec".len() as u64);
    }

    #[test]
    fn digest_policies_fail_closed_when_frozen_bytes_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("truncated.jsonl");
        std::fs::write(&path, b"complete\n").unwrap();
        let mut hashed_reader = BufReader::new(File::open(&path).unwrap());
        let mut unhashed_reader = BufReader::new(File::open(&path).unwrap());
        let mut hashed_storage = Vec::new();
        let mut unhashed_storage = Vec::new();
        let mut full_hasher = Sha256::new();
        let mut complete_hasher = Sha256::new();

        read_bounded_record(
            &mut hashed_reader,
            &mut hashed_storage,
            &mut full_hasher,
            &mut complete_hasher,
            10,
        )
        .unwrap();
        read_bounded_record_unhashed(&mut unhashed_reader, &mut unhashed_storage, 10).unwrap();
        let hashed_error = match read_bounded_record(
            &mut hashed_reader,
            &mut hashed_storage,
            &mut full_hasher,
            &mut complete_hasher,
            1,
        ) {
            Err(error) => error,
            Ok(_) => panic!("hashed reader accepted missing frozen bytes"),
        };
        let unhashed_error =
            match read_bounded_record_unhashed(&mut unhashed_reader, &mut unhashed_storage, 1) {
                Err(error) => error,
                Ok(_) => panic!("unhashed reader accepted missing frozen bytes"),
            };

        assert_eq!(unhashed_error.to_string(), hashed_error.to_string());
        assert!(unhashed_error
            .to_string()
            .contains("source changed while NativePath was reading it"));
    }
}

pub(super) fn trim_jsonl_terminator(mut record: &[u8]) -> &[u8] {
    if record.last() == Some(&b'\r') {
        record = &record[..record.len() - 1];
    }
    record
}

pub(super) struct ValidatedCheckpoint {
    pub(super) bytes_read: u64,
    pub(super) complete_prefix_hasher: Sha256,
    pub(super) complete_prefix_ends_with_terminal_nul_padding: bool,
    pub(super) pending_tool_contexts: BTreeMap<String, CodexToolCallContext>,
    pub(super) pending_tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    pub(super) pending_continuations: BTreeMap<String, String>,
}

pub(super) fn decode_pending_tool_authority(
    record: &[u8],
    authority: &CodexPendingToolAuthority,
    owner: &CodexSessionRow,
) -> Result<(String, CodexToolCallContext)> {
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
    let row = match build_event_row(authority.raw_ordinal, kind, &retained)? {
        Ok(row) => row,
        Err(
            CodexRetainedNonMaterialized::ValidUnmaterializable
            | CodexRetainedNonMaterialized::Malformed,
        ) => {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority cannot be projected",
            ));
        }
    };
    let (call_id, mut context) = tool_context_from_row(&row).ok_or_else(|| {
        invalid_checkpoint_proof("pending tool-call authority has no correlation identity")
    })?;
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
    Ok((call_id, bound_tool_context(context)))
}

pub(super) fn validate_checkpoint_source(
    reader: &mut BufReader<File>,
    checkpoint: &CodexNativeCheckpoint,
    append_replay: bool,
) -> Result<ValidatedCheckpoint> {
    // The prefix proof is the sole read pass over checkpointed bytes. On
    // append, only the at-most-24 authority spans are retained long enough to
    // reconstruct transient correlation state during that same pass.
    reader.seek(SeekFrom::Start(0))?;
    let complete_prefix_end = checkpoint.complete_prefix_end();
    let mut remaining = checkpoint.observation.len;
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; CHECKPOINT_READ_BUFFER_BYTES];
    let mut full_hasher = Sha256::new();
    let mut complete_prefix_hasher = Sha256::new();
    let mut incomplete_tail_hasher = Sha256::new();
    let mut complete_records = 0_u64;
    let mut final_prefix_byte = None;
    let mut terminal_suffix_all_nul = true;
    let mut terminal_suffix_len = 0_u64;
    let mut tail_contains_newline = false;
    let mut authorities = checkpoint
        .pending_tool_authorities()
        .iter()
        .collect::<Vec<_>>();
    authorities.sort_by_key(|authority| authority.record_start);
    let mut authority_index = 0_usize;
    let mut current_record_start = 0_u64;
    let mut pending_tool_record = Vec::new();
    let mut pending_tool_contexts = BTreeMap::new();
    let mut pending_tool_authorities = BTreeMap::new();
    let mut pending_continuations = BTreeMap::new();

    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(CHECKPOINT_READ_BUFFER_BYTES as u64))
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds usize"))?;
        let read = reader.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(invalid_checkpoint_proof(
                "checkpoint observation ends after source EOF",
            ));
        }
        let chunk = &buffer[..read];
        full_hasher.update(chunk);
        let read_u64 = u64::try_from(read)
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds u64"))?;
        let chunk_end = offset
            .checked_add(read_u64)
            .ok_or(CaptureError::SystemInvariant(
                "Codex checkpoint offset exceeds u64",
            ))?;

        if offset < complete_prefix_end {
            let prefix_len = usize::try_from((complete_prefix_end.min(chunk_end)) - offset)
                .map_err(|_| CaptureError::SystemInvariant("Codex prefix length exceeds usize"))?;
            let prefix = &chunk[..prefix_len];
            complete_prefix_hasher.update(prefix);
            for (index, byte) in prefix.iter().enumerate() {
                let absolute_offset = offset
                    .checked_add(u64::try_from(index).unwrap_or(u64::MAX))
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex checkpoint record offset exceeds u64",
                    ))?;
                if append_replay
                    && authorities.get(authority_index).is_some_and(|authority| {
                        absolute_offset >= authority.record_start
                            && absolute_offset < authority.record_end
                    })
                {
                    pending_tool_record.push(*byte);
                }
                if *byte != b'\n' {
                    terminal_suffix_all_nul &= *byte == 0;
                    terminal_suffix_len = terminal_suffix_len.saturating_add(1);
                    continue;
                }
                let record_end =
                    absolute_offset
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Codex checkpoint record boundary exceeds u64",
                        ))?;
                if let Some(authority) = authorities.get(authority_index) {
                    if authority.record_start < record_end {
                        if authority.record_start != current_record_start
                            || authority.record_end != record_end
                            || authority.raw_ordinal != complete_records
                        {
                            return Err(invalid_checkpoint_proof(
                                "pending tool-call authority does not match its JSONL record boundary",
                            ));
                        }
                        if append_replay {
                            let (call_id, context) = decode_pending_tool_authority(
                                pending_tool_record.as_slice(),
                                authority,
                                &checkpoint.owner,
                            )?;
                            if pending_tool_contexts
                                .insert(call_id.clone(), context)
                                .is_some()
                                || pending_tool_authorities
                                    .insert(call_id, (*authority).clone())
                                    .is_some()
                            {
                                return Err(invalid_checkpoint_proof(
                                    "pending tool-call authority correlation is duplicated",
                                ));
                            }
                            pending_tool_record.clear();
                        }
                        authority_index = authority_index.saturating_add(1);
                    }
                }
                current_record_start = record_end;
                complete_records = complete_records.saturating_add(1);
                terminal_suffix_all_nul = true;
                terminal_suffix_len = 0;
            }
            final_prefix_byte = prefix.last().copied().or(final_prefix_byte);
            if prefix_len < chunk.len() {
                let tail = &chunk[prefix_len..];
                incomplete_tail_hasher.update(tail);
                tail_contains_newline |= tail.contains(&b'\n');
            }
        } else {
            incomplete_tail_hasher.update(chunk);
            tail_contains_newline |= chunk.contains(&b'\n');
        }
        offset = chunk_end;
        remaining -= read_u64;
    }

    let full_revision_sha256: [u8; 32] = full_hasher.finalize().into();
    let complete_prefix_sha256: [u8; 32] = complete_prefix_hasher.clone().finalize().into();
    let complete_prefix_ends_with_terminal_nul_padding =
        terminal_suffix_len != 0 && terminal_suffix_all_nul;
    if complete_prefix_ends_with_terminal_nul_padding {
        complete_records = complete_records.saturating_add(1);
    }
    if full_revision_sha256 != checkpoint.full_revision_sha256
        || complete_prefix_sha256 != checkpoint.complete_prefix_sha256
        || complete_records != checkpoint.next_raw_ordinal()
        || authority_index != authorities.len()
        || (complete_prefix_end != 0
            && final_prefix_byte != Some(b'\n')
            && !complete_prefix_ends_with_terminal_nul_padding)
    {
        return Err(invalid_checkpoint_proof(
            "checkpoint digest, boundary, or raw ordinal does not match source bytes",
        ));
    }

    match checkpoint.incomplete_tail() {
        None if complete_prefix_end == checkpoint.observation.len => {}
        Some((tail_len, tail_sha256))
            if !tail_contains_newline
                && tail_len == checkpoint.observation.len - complete_prefix_end
                && <[u8; 32]>::from(incomplete_tail_hasher.finalize()) == tail_sha256 => {}
        _ => {
            return Err(invalid_checkpoint_proof(
                "checkpoint incomplete-tail proof does not match source bytes",
            ));
        }
    }
    if append_replay {
        for (call_id, authority) in &pending_tool_authorities {
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
        }
    }

    Ok(ValidatedCheckpoint {
        bytes_read: checkpoint.observation.len,
        complete_prefix_hasher,
        complete_prefix_ends_with_terminal_nul_padding,
        pending_tool_contexts,
        pending_tool_authorities,
        pending_continuations,
    })
}

pub(super) fn invalid_checkpoint_proof(reason: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid Codex append proof: {reason}"))
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

pub(crate) fn open_codex_source_capability(
    source: &CodexCatalogSource,
) -> Result<Arc<OpenedProviderSourceFile>> {
    if let Some(opened) = source.opened.as_ref() {
        return Ok(Arc::clone(opened));
    }
    reopen_codex_source_capability(source)
}

/// Reopens the authority-relative directory entry instead of consulting a
/// previously retained leaf capability. Generation preparation uses this to
/// prove that the path still names the cataloged ordinary file before any
/// route worker can consume its child-local source plan.
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
