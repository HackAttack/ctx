//! Bounded per-session Hermes projection and certification.

use super::*;

const HERMES_SESSION_KEY_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub(super) struct HermesSessionContext {
    pub(super) session_id: StableEntityId,
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) agent_scope: AgentScope,
    pub(super) branch: Option<String>,
    pub(super) workspace: Option<String>,
    pub(super) cwd: Option<String>,
}

fn direct_session_context(
    profile_source: &SourceKey,
    session_source: &SourceKey,
    row: &HermesSessionRow,
) -> Result<HermesSessionContext, CaptureError> {
    validate_session_key(&row.id)?;
    provider_required_timestamp_seconds(row.started_at, "Hermes session started_at")?;
    row.ended_at
        .map(|value| provider_required_timestamp_seconds(value, "Hermes session ended_at"))
        .transpose()?;

    let expected_source = hermes_session_source_key(profile_source, &row.id)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if !expected_source.exact_descriptor_eq(session_source) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let session_id = hermes_session_id(session_source, &row.id)?;
    let parent_session_id = row
        .parent_session_id
        .as_deref()
        .map(|parent| {
            validate_session_key(parent)?;
            let parent_source = hermes_session_source_key(profile_source, parent)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            hermes_session_id(&parent_source, parent)
        })
        .transpose()?;
    Ok(HermesSessionContext {
        session_id,
        parent_session_id,
        agent_scope: if parent_session_id.is_some() {
            AgentScope::Subagent
        } else {
            AgentScope::Primary
        },
        branch: row.git_branch.clone(),
        workspace: row.git_repo_root.clone(),
        cwd: row.cwd.clone(),
    })
}

fn hermes_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> Result<StableEntityId, CaptureError> {
    let session_key = NativeSessionKey::native_id(
        HERMES_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: HERMES_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn validate_session_key(value: &str) -> Result<(), CaptureError> {
    if value.len() > HERMES_SESSION_KEY_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "Hermes session identifier exceeds the {HERMES_SESSION_KEY_MAX_BYTES}-byte Core key bound"
        )));
    }
    Ok(())
}

pub(super) struct HermesSessionProjection {
    pub(super) certificate: CertifiedSource,
    pub(super) decoded_rows: u64,
    pub(super) emitted_pages: u64,
    pub(super) peak_buffered_records: u64,
    pub(super) native_candidate_query_batches: u64,
    pub(super) native_hydration_query_batches: u64,
    pub(super) max_native_rows_per_set: u64,
}

pub(super) enum HermesSnapshotProjectionOutput {
    Page(HermesSourceBackedPage),
    Progress(SourceBackedCurrentSourceProgress),
}

#[cfg(test)]
pub(super) fn project_hermes_session_snapshot<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    leaf: &HermesSessionLeaf<L>,
    schema: &HermesSchema,
    conn: &rusqlite::Connection,
    message_spool: &mut HermesExactMessageSpool,
    emit: &mut dyn FnMut(HermesSourceBackedPage) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSessionProjection>
where
    L::PinnedAppendBase: Clone,
{
    project_hermes_session_snapshot_with_progress(
        candidate,
        leaf,
        schema,
        conn,
        message_spool,
        &mut |output| match output {
            HermesSnapshotProjectionOutput::Page(page) => emit(page),
            HermesSnapshotProjectionOutput::Progress(_) => Ok(()),
        },
    )
}

pub(super) fn project_hermes_session_snapshot_with_progress<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    leaf: &HermesSessionLeaf<L>,
    schema: &HermesSchema,
    conn: &rusqlite::Connection,
    message_spool: &mut HermesExactMessageSpool,
    emit: &mut dyn FnMut(HermesSnapshotProjectionOutput) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSessionProjection>
where
    L::PinnedAppendBase: Clone,
{
    leaf.source.validate_contract()?;
    let source_path = candidate
        .path
        .to_str()
        .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(candidate.path.clone()))?
        .to_owned();
    let mut reader = HermesRowReader::for_session(conn, schema, &leaf.provider_session_id)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut message_replay = leaf
        .exact_message_range
        .map(|range| message_spool.prepare_replay(range))
        .transpose()
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut context = None;
    let mut context_rejection = None;
    let operation: HermesSourceBackedResult<(ScannedSourceCounts, [u8; 32], u64, u64, u64)> =
        (|| {
            let mut frontier = super::super::sqlite::HermesFrontier::initial();
            let mut digest = Sha256::new();
            digest.update(HERMES_SOURCE_DIGEST_DOMAIN);
            let mut counts = ScannedSourceCounts::default();
            let mut page_records = Vec::new();
            let mut page_owned_bytes = 0_usize;
            let mut page_completed_bytes = 0_u64;
            let mut decoded_rows = 0_u64;
            let mut emitted_pages = 0_u64;
            let mut peak_buffered_records = 0_u64;
            emit(HermesSnapshotProjectionOutput::Progress(
                hermes_logical_progress(SourceBackedCurrentSourceProgressStage::LogicalScan, 0, 0),
            ))?;

            let mut session_read = false;
            let mut consumed_messages = 0_u64;
            loop {
                let native_page = if !session_read {
                    session_read = true;
                    let page = reader.next_session_inventory_page(None)?;
                    if page.is_empty() {
                        if message_replay.is_some() {
                            continue;
                        }
                        break;
                    }
                    page
                } else if let Some(replay) = message_replay.as_mut() {
                    reader.exact_message_page(replay, consumed_messages, frontier.next_ordinal)?
                } else {
                    Vec::new()
                };
                if native_page.is_empty() {
                    break;
                }
                if session_read && native_page[0].locator.phase == HermesPhase::Messages {
                    consumed_messages = checked_add(
                        consumed_messages,
                        u64::try_from(native_page.len())
                            .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                    )?;
                }
                frontier = native_page
                    .last()
                    .map(|native| native.next_frontier)
                    .unwrap_or(frontier);
                for native in native_page {
                    decoded_rows = checked_add(decoded_rows, 1)?;
                    counts.complete_records = checked_add(counts.complete_records, 1)?;
                    let observed_bytes = u64::try_from(native.observed_bytes)
                        .map_err(|_| HermesSourceBackedError::CountOverflow)?;
                    counts.certified_bytes = checked_add(counts.certified_bytes, observed_bytes)?;

                    let logical_digest = native_record_digest(&native)?;
                    digest.update([match native.locator.phase {
                        HermesPhase::Sessions => 1,
                        HermesPhase::Messages => 2,
                    }]);
                    digest.update(native.ordinal.to_be_bytes());
                    digest.update(observed_bytes.to_be_bytes());
                    digest.update(logical_digest);

                    if let HermesNativeRecord::Session(row) = &native.record {
                        match direct_session_context(&candidate.source, &leaf.source, row) {
                            Ok(resolved) => context = Some(resolved),
                            Err(CaptureError::InvalidPayload(reason)) => {
                                context_rejection = Some(reason)
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    let record = project_native_row(
                        &leaf.source,
                        &source_path,
                        native,
                        context.as_ref(),
                        context_rejection.as_deref(),
                    )?;
                    let (record, owned_bytes) = bound_projected_record(record)?;

                    match &record {
                        HermesSourceBackedRecord::Session(_) => {
                            counts.retained_records = checked_add(counts.retained_records, 1)?;
                        }
                        HermesSourceBackedRecord::Event(_) => {
                            counts.retained_records = checked_add(counts.retained_records, 1)?;
                            counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                        }
                        HermesSourceBackedRecord::Rejected(_) => {
                            counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                        }
                    }

                    if !page_records.is_empty()
                        && (page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                            || page_owned_bytes.saturating_add(owned_bytes)
                                > NATIVE_INGESTION_PAGE_MAX_BYTES)
                    {
                        let records = std::mem::take(&mut page_records);
                        peak_buffered_records = peak_buffered_records.max(
                            u64::try_from(records.len())
                                .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                        );
                        emit(HermesSnapshotProjectionOutput::Page(
                            HermesSourceBackedPage {
                                records,
                                completed_bytes: page_completed_bytes,
                            },
                        ))?;
                        emitted_pages = checked_add(emitted_pages, 1)?;
                        page_owned_bytes = 0;
                        page_completed_bytes = 0;
                    }
                    page_owned_bytes = page_owned_bytes.saturating_add(owned_bytes);
                    page_completed_bytes = checked_add(page_completed_bytes, observed_bytes)?;
                    page_records.push(record);
                    if page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS {
                        let records = std::mem::take(&mut page_records);
                        peak_buffered_records = peak_buffered_records.max(
                            u64::try_from(records.len())
                                .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                        );
                        emit(HermesSnapshotProjectionOutput::Page(
                            HermesSourceBackedPage {
                                records,
                                completed_bytes: page_completed_bytes,
                            },
                        ))?;
                        emitted_pages = checked_add(emitted_pages, 1)?;
                        page_owned_bytes = 0;
                        page_completed_bytes = 0;
                    }
                }
                emit(HermesSnapshotProjectionOutput::Progress(
                    hermes_logical_progress(
                        SourceBackedCurrentSourceProgressStage::LogicalScan,
                        counts.complete_records,
                        counts.certified_bytes,
                    ),
                ))?;
            }
            if !page_records.is_empty() {
                peak_buffered_records = peak_buffered_records.max(
                    u64::try_from(page_records.len())
                        .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                );
                emit(HermesSnapshotProjectionOutput::Page(
                    HermesSourceBackedPage {
                        records: page_records,
                        completed_bytes: page_completed_bytes,
                    },
                ))?;
                emitted_pages = checked_add(emitted_pages, 1)?;
            }
            emit(HermesSnapshotProjectionOutput::Progress(
                hermes_logical_progress(
                    SourceBackedCurrentSourceProgressStage::LogicalScan,
                    counts.complete_records,
                    counts.certified_bytes,
                ),
            ))?;
            Ok((
                counts,
                digest.finalize().into(),
                decoded_rows,
                emitted_pages,
                peak_buffered_records,
            ))
        })();
    let reader_counters = reader.counters();
    drop(reader);

    let (counts, content_digest, decoded_rows, emitted_pages, peak_buffered_records) = operation
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let observation = SourceObservation::new(
        leaf.source.clone(),
        HERMES_SESSION_OBSERVATION_KIND,
        leaf.observation_revision.clone(),
    )?;
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        HERMES_SOURCE_PARSER_REVISION,
        content_digest,
        counts,
    )?;
    #[cfg(any(test, feature = "test-support"))]
    record_logical_row_traversal();
    record_session_scan_receipt(
        &leaf.provider_session_id,
        decoded_rows,
        reader_counters.hydration_query_batches,
    );
    Ok(HermesSessionProjection {
        certificate,
        decoded_rows,
        emitted_pages,
        peak_buffered_records,
        native_candidate_query_batches: reader_counters.candidate_query_batches,
        native_hydration_query_batches: reader_counters.hydration_query_batches,
        max_native_rows_per_set: reader_counters.max_hydration_rows,
    })
}

pub(super) fn project_hermes_incremental_leaf_with_progress<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    leaf: &HermesSessionLeaf<L>,
    incremental: &HermesIncrementalLeaf<L>,
    emit: &mut dyn FnMut(HermesSnapshotProjectionOutput) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSessionProjection>
where
    L::PinnedAppendBase: Clone,
{
    leaf.source.validate_contract()?;
    let source_path = candidate
        .path
        .to_str()
        .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(candidate.path.clone()))?
        .to_owned();
    let (context, context_rejection) = match &incremental.session.record {
        HermesNativeRecord::Session(row) => {
            match direct_session_context(&candidate.source, &leaf.source, row) {
                Ok(context) => (Some(context), None),
                Err(CaptureError::InvalidPayload(reason)) => (None, Some(reason)),
                Err(error) => return Err(error.into()),
            }
        }
        HermesNativeRecord::Rejected(reason) => (None, Some(reason.clone())),
        HermesNativeRecord::Message { .. } => {
            return Err(CaptureError::SystemInvariant(
                "Hermes incremental session context was a message row",
            )
            .into())
        }
    };
    let mut rows = Vec::with_capacity(
        incremental
            .messages
            .len()
            .saturating_add(usize::from(incremental.base.is_none())),
    );
    if incremental.base.is_none() {
        rows.push(incremental.session.clone());
    }
    rows.extend(incremental.messages.iter().cloned());

    let mut digest = Sha256::new();
    let mut counts = incremental
        .base
        .as_ref()
        .map_or_else(ScannedSourceCounts::default, |base| {
            base.certificate().counts()
        });
    if let Some(base) = incremental.base.as_ref() {
        digest.update(HERMES_INCREMENTAL_CONTENT_DOMAIN);
        digest.update(base.certificate().content_digest());
    } else {
        digest.update(HERMES_SOURCE_DIGEST_DOMAIN);
    }
    let mut page_records = Vec::new();
    let mut page_owned_bytes = 0_usize;
    let mut page_completed_bytes = 0_u64;
    let mut emitted_pages = 0_u64;
    let mut peak_buffered_records = 0_u64;
    for native in rows {
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        let observed_bytes = u64::try_from(native.observed_bytes)
            .map_err(|_| HermesSourceBackedError::CountOverflow)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, observed_bytes)?;
        let logical_digest = native_record_digest(&native)?;
        digest.update([match native.locator.phase {
            HermesPhase::Sessions => 1,
            HermesPhase::Messages => 2,
        }]);
        digest.update(native.ordinal.to_be_bytes());
        digest.update(observed_bytes.to_be_bytes());
        digest.update(logical_digest);
        let record = project_native_row(
            &leaf.source,
            &source_path,
            native,
            context.as_ref(),
            context_rejection.as_deref(),
        )?;
        let (record, owned_bytes) = bound_projected_record(record)?;
        match &record {
            HermesSourceBackedRecord::Session(_) => {
                counts.retained_records = checked_add(counts.retained_records, 1)?;
            }
            HermesSourceBackedRecord::Event(_) => {
                counts.retained_records = checked_add(counts.retained_records, 1)?;
                counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
            }
            HermesSourceBackedRecord::Rejected(_) => {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
            }
        }
        if !page_records.is_empty()
            && (page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                || page_owned_bytes.saturating_add(owned_bytes) > NATIVE_INGESTION_PAGE_MAX_BYTES)
        {
            peak_buffered_records = peak_buffered_records.max(page_records.len() as u64);
            emit(HermesSnapshotProjectionOutput::Page(
                HermesSourceBackedPage {
                    records: std::mem::take(&mut page_records),
                    completed_bytes: page_completed_bytes,
                },
            ))?;
            emitted_pages = checked_add(emitted_pages, 1)?;
            page_owned_bytes = 0;
            page_completed_bytes = 0;
        }
        page_owned_bytes = page_owned_bytes.saturating_add(owned_bytes);
        page_completed_bytes = checked_add(page_completed_bytes, observed_bytes)?;
        page_records.push(record);
    }
    if !page_records.is_empty() {
        peak_buffered_records = peak_buffered_records.max(page_records.len() as u64);
        emit(HermesSnapshotProjectionOutput::Page(
            HermesSourceBackedPage {
                records: page_records,
                completed_bytes: page_completed_bytes,
            },
        ))?;
        emitted_pages = checked_add(emitted_pages, 1)?;
    }
    let observation = SourceObservation::new(
        leaf.source.clone(),
        HERMES_SESSION_OBSERVATION_KIND,
        leaf.observation_revision.clone(),
    )?;
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        HERMES_SOURCE_PARSER_REVISION,
        digest.finalize().into(),
        counts,
    )?;
    #[cfg(any(test, feature = "test-support"))]
    record_logical_row_traversal();
    record_session_scan_receipt(
        &leaf.provider_session_id,
        counts.complete_records.saturating_sub(
            incremental
                .base
                .as_ref()
                .map_or(0, |base| base.certificate().counts().complete_records),
        ),
        1,
    );
    Ok(HermesSessionProjection {
        certificate,
        decoded_rows: counts.complete_records,
        emitted_pages,
        peak_buffered_records,
        native_candidate_query_batches: 1,
        native_hydration_query_batches: 1,
        max_native_rows_per_set: incremental.messages.len().min(64) as u64,
    })
}

pub(super) fn diagnose_hermes_query_error(
    error: HermesSourceBackedError,
    phase: SqliteFailurePhase,
) -> HermesSourceBackedError {
    match error {
        HermesSourceBackedError::Capture(CaptureError::Sqlite(source)) => {
            SqliteSourceAccessError::Sqlite {
                operation: match phase {
                    SqliteFailurePhase::Schema => "probing the Hermes SQLite schema",
                    _ => "projecting the Hermes SQLite snapshot",
                },
                source,
            }
            .with_diagnostic(
                phase,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                0,
                SqliteCleanupStatus::NotRequired,
            )
            .into()
        }
        error => error,
    }
}

pub(super) fn checked_add(left: u64, right: u64) -> HermesSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(HermesSourceBackedError::CountOverflow)
}
