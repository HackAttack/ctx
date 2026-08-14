use super::*;
use crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES;

#[derive(Debug)]
struct TouchCandidate {
    line_number: usize,
    source_id: String,
    session_id: String,
    event_index: Option<u64>,
}

#[derive(Debug)]
struct EdgeCandidate {
    line_number: usize,
    source_id: String,
    from_session_id: String,
    to_session_id: String,
    edge_type: SessionEdgeType,
}

#[derive(Debug)]
struct ProjectionCatalog {
    summary: ProviderImportSummary,
    manifest_line: Option<usize>,
    manifest_failure: Option<(ProviderSourceFailureKind, String)>,
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    sources: BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    events: BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    touch_keys: BTreeSet<(String, String, u64)>,
    touches: Vec<TouchCandidate>,
    edge_keys: BTreeSet<(String, String, String, String)>,
    edges: Vec<EdgeCandidate>,
    oversized_lines: BTreeSet<usize>,
    budget: CatalogBudget,
}

impl ProjectionCatalog {
    fn new(limits: CustomHistoryCatalogLimits) -> Self {
        Self {
            summary: ProviderImportSummary::default(),
            manifest_line: None,
            manifest_failure: None,
            lineage_contract: None,
            sources: BTreeMap::new(),
            sessions: BTreeMap::new(),
            events: BTreeMap::new(),
            touch_keys: BTreeSet::new(),
            touches: Vec::new(),
            edge_keys: BTreeSet::new(),
            edges: Vec::new(),
            oversized_lines: BTreeSet::new(),
            budget: CatalogBudget::new(limits),
        }
    }
}

pub(super) fn parse_projection(
    source: &OpenedProviderSourceFile,
    prior_prefix_bytes: Option<u64>,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    parse_projection_with_limits(
        source,
        prior_prefix_bytes,
        CustomHistoryCatalogLimits::PRODUCTION,
    )
}

pub(super) fn parse_projection_with_limits(
    source: &OpenedProviderSourceFile,
    prior_prefix_bytes: Option<u64>,
    limits: CustomHistoryCatalogLimits,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.projection_parses = work.projection_parses.saturating_add(1);
        work.source_read_passes = work.source_read_passes.saturating_add(1);
    });

    let frozen_length = source.len();
    let mut event_spool = tempfile::tempfile()?;
    let mut catalog = ProjectionCatalog::new(limits);
    let mut stream = JsonlPhysicalStream::open(
        source.file().try_clone()?,
        frozen_length,
        0,
        0,
        JsonlRecordFraming::new(MAX_PROVIDER_JSONL_LINE_BYTES.saturating_sub(1), false),
        JsonlPhysicalDigest::complete_and_bounded_prefix(
            new_complete_prefix_hasher(),
            new_prefix_hasher(),
            prior_prefix_bytes.unwrap_or(0),
        ),
        || CaptureError::SourceChangedDuringCapture,
    )?;

    {
        let mut event_writer = BufWriter::new(&mut event_spool);
        while let Some(record) = stream.next_record()? {
            #[cfg(test)]
            record_custom_history_work(|work| {
                work.peak_provider_record_bytes =
                    work.peak_provider_record_bytes.max(record.stored_len);
            });
            if !record.complete {
                break;
            }

            let line_number = usize::try_from(record.physical_ordinal.saturating_add(1))
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
            let evidence = CompleteLine {
                line_number,
                byte_offset: record.byte_start,
            };
            catalog.budget.admit_record()?;
            if record.oversized {
                catalog.summary.skipped = catalog.summary.skipped.saturating_add(1);
                catalog.summary.skipped_events = catalog.summary.skipped_events.saturating_add(1);
                catalog.oversized_lines.insert(line_number);
            } else {
                visit_record(
                    stream.record_bytes(record),
                    evidence,
                    &mut catalog,
                    &mut event_writer,
                )?;
            }
        }
        event_writer.flush()?;
    }

    event_spool.seek(SeekFrom::Start(0))?;
    let terminal = stream.terminal();
    let certified_prefix_bytes = stream.complete_prefix_end();
    let complete_records = usize::try_from(stream.next_physical_ordinal())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let source_hasher = stream.digest().complete_hasher();
    let content_digest = finish_complete_prefix_digest(source_hasher, certified_prefix_bytes);
    let (prior_hasher, prior_remaining) = stream
        .digest()
        .bounded_prefix()
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let observed_prior_prefix_digest = match prior_prefix_bytes {
        Some(expected) if prior_remaining == 0 => {
            Some(finish_prefix_digest(prior_hasher, expected))
        }
        _ => None,
    };
    finish_projection(
        catalog,
        event_spool,
        certified_prefix_bytes,
        complete_records,
        terminal,
        content_digest,
        observed_prior_prefix_digest,
        prior_prefix_bytes,
    )
}

fn visit_record(
    bytes: &[u8],
    line: CompleteLine,
    catalog: &mut ProjectionCatalog,
    event_writer: &mut impl Write,
) -> CustomHistorySourceBackedResult<()> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.provider_records_parsed = work.provider_records_parsed.saturating_add(1);
    });
    let record = match serde_json::from_slice::<CtxHistoryJsonlRecord>(bytes) {
        Ok(record) => record,
        Err(error) => {
            push_provider_import_failure(&mut catalog.summary, line.line_number, error.to_string());
            return Ok(());
        }
    };
    match record {
        CtxHistoryJsonlRecord::Manifest(manifest) => {
            if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                catalog.manifest_failure.get_or_insert_with(|| {
                    (
                        ProviderSourceFailureKind::SchemaIncompatible,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    )
                });
            }
            if catalog.manifest_line.replace(line.line_number).is_some() {
                catalog.manifest_failure = Some((
                    ProviderSourceFailureKind::InvalidSource,
                    format!("duplicate manifest record at line {}", line.line_number),
                ));
            }
            catalog.lineage_contract = manifest.lineage_contract;
        }
        CtxHistoryJsonlRecord::Source(source) => {
            let failures_before = catalog.summary.failed;
            validate_custom_source_record(&mut catalog.summary, line.line_number, &source);
            if catalog.sources.contains_key(&source.source_id) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate source_id".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    source.source_id.len(),
                    source.provider_key.len(),
                ]))?;
                catalog.sources.insert(
                    source.source_id.clone(),
                    CustomSourceCatalogEntry {
                        provider_key: source.provider_key,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Session(session) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::NativeSessionIdBytes,
                session.native_session_id.as_deref(),
            )?;
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::ParentSessionIdBytes,
                session.parent_session_id.as_deref(),
            )?;
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::RootSessionIdBytes,
                session.root_session_id.as_deref(),
            )?;
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &session.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &session.session_id,
            );
            let key = (session.source_id.clone(), session.session_id.clone());
            if catalog.sessions.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate session record".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let agent_type = session.agent_type.as_str().to_owned();
                let cwd = session.cwd.as_deref().and_then(bounded_metadata);
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    session.source_id.len().saturating_mul(2),
                    session.session_id.len().saturating_mul(2),
                    session.native_session_id.as_ref().map_or(0, String::len),
                    session.parent_session_id.as_ref().map_or(0, String::len),
                    session.root_session_id.as_ref().map_or(0, String::len),
                    agent_type.len(),
                    cwd.as_ref().map_or(0, String::len),
                ]))?;
                catalog.sessions.insert(
                    key,
                    CustomSessionCatalogEntry {
                        line_number: line.line_number,
                        source_id: session.source_id,
                        session_id: session.session_id,
                        native_session_id: session.native_session_id,
                        parent_session_id: session.parent_session_id,
                        root_session_id: session.root_session_id,
                        session_relationship: session.session_relationship,
                        agent_type,
                        cwd,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::Event(event) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::EventIdBytes,
                event.event_id.as_deref(),
            )?;
            if let Some(copied_from) = &event.copied_from {
                ensure_retained_key_bound(
                    CustomHistorySourceBackedBound::NativeSessionIdBytes,
                    Some(&copied_from.ancestor_native_session_id),
                )?;
                ensure_retained_key_bound(
                    CustomHistorySourceBackedBound::EventIdBytes,
                    Some(&copied_from.ancestor_event_id),
                )?;
            }
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &event.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &event.session_id,
            );
            let key = (
                event.source_id.clone(),
                event.session_id.clone(),
                event.event_index,
            );
            if catalog.events.contains_key(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate event_index for session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                let event_id = event.event_id.clone();
                let copied_from = event.copied_from.clone();
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    event.source_id.len(),
                    event.session_id.len(),
                    event.event_id.as_ref().map_or(0, String::len),
                    event.copied_from.as_ref().map_or(0, |selector| {
                        selector
                            .ancestor_native_session_id
                            .len()
                            .saturating_add(selector.ancestor_event_id.len())
                    }),
                ]))?;
                let body = lexical_body(&event);
                #[cfg(test)]
                record_custom_history_work(|work| {
                    work.spooled_event_body_bytes =
                        work.spooled_event_body_bytes.saturating_add(body.len());
                    work.resident_event_body_bytes = body.len();
                    work.peak_resident_event_body_bytes =
                        work.peak_resident_event_body_bytes.max(body.len());
                });
                write_spooled_event(
                    event_writer,
                    &SpooledCustomEvent {
                        source_id: event.source_id,
                        session_id: event.session_id,
                        event_index: event.event_index,
                        event_id: event_id.clone(),
                        event_type: event.event_type.as_str().to_owned(),
                        role: event.role.map(|role| role.as_str().to_owned()),
                        occurred_at_unix_ms: event.occurred_at.timestamp_millis(),
                        body,
                    },
                )?;
                #[cfg(test)]
                record_custom_history_work(|work| {
                    work.resident_event_body_bytes = 0;
                });
                catalog.events.insert(
                    key,
                    CustomEventCatalogEntry {
                        line_number: line.line_number,
                        line,
                        event_id,
                        copied_from,
                    },
                );
            }
        }
        CtxHistoryJsonlRecord::FileTouch(touch) => {
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &touch.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "session_id",
                &touch.session_id,
            );
            if touch.path.trim().is_empty() {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "file_touch path must not be empty".to_owned(),
                );
            }
            let key = (
                touch.source_id.clone(),
                touch.session_id.clone(),
                touch.touch_index,
            );
            if catalog.touch_keys.contains(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate touch_index for session".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    touch.source_id.len().saturating_mul(2),
                    touch.session_id.len().saturating_mul(2),
                ]))?;
                catalog.touch_keys.insert(key);
                catalog.touches.push(TouchCandidate {
                    line_number: line.line_number,
                    source_id: touch.source_id,
                    session_id: touch.session_id,
                    event_index: touch.event_index,
                });
            }
        }
        CtxHistoryJsonlRecord::Edge(edge) => {
            ensure_retained_key_bound(
                CustomHistorySourceBackedBound::EdgeIdBytes,
                edge.edge_id.as_deref(),
            )?;
            let failures_before = catalog.summary.failed;
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "source_id",
                &edge.source_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "from_session_id",
                &edge.from_session_id,
            );
            validate_custom_history_identifier(
                &mut catalog.summary,
                line.line_number,
                "to_session_id",
                &edge.to_session_id,
            );
            let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                format!(
                    "{}:{}:{}",
                    edge.from_session_id,
                    edge.to_session_id,
                    edge.edge_type.as_str()
                )
            });
            let key = (
                edge.source_id.clone(),
                edge.from_session_id.clone(),
                edge.to_session_id.clone(),
                edge_key,
            );
            if catalog.edge_keys.contains(&key) {
                push_provider_import_failure(
                    &mut catalog.summary,
                    line.line_number,
                    "duplicate edge record".to_owned(),
                );
            }
            if catalog.summary.failed == failures_before {
                catalog.budget.admit_metadata(retained_metadata_bytes(&[
                    edge.source_id.len().saturating_mul(2),
                    edge.from_session_id.len().saturating_mul(2),
                    edge.to_session_id.len().saturating_mul(2),
                    key.3.len(),
                ]))?;
                catalog.edge_keys.insert(key);
                catalog.edges.push(EdgeCandidate {
                    line_number: line.line_number,
                    source_id: edge.source_id,
                    from_session_id: edge.from_session_id,
                    to_session_id: edge.to_session_id,
                    edge_type: edge.edge_type,
                });
            }
        }
    }
    Ok(())
}

fn retained_metadata_bytes(lengths: &[usize]) -> usize {
    lengths.iter().fold(
        CUSTOM_HISTORY_CATALOG_ENTRY_OVERHEAD_BYTES,
        |total, length| total.saturating_add(*length),
    )
}

fn ensure_retained_key_bound(
    limit: CustomHistorySourceBackedBound,
    value: Option<&str>,
) -> CustomHistorySourceBackedResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES {
        return Err(CustomHistorySourceBackedError::Bounds {
            limit,
            maximum: CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES,
            observed: value.len(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_projection(
    mut catalog: ProjectionCatalog,
    event_spool: File,
    certified_prefix_bytes: u64,
    complete_records: usize,
    terminal: bool,
    content_digest: [u8; 32],
    observed_prior_prefix_digest: Option<[u8; 32]>,
    prior_prefix_bytes: Option<u64>,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    if catalog.manifest_line.is_none() {
        catalog.manifest_failure = Some((
            ProviderSourceFailureKind::InvalidSource,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        ));
    }
    if let Some((kind, detail)) = catalog.manifest_failure {
        return Err(CustomHistorySourceBackedError::StructuralManifest { kind, detail });
    }
    apply_session_lineage_contract(&mut catalog);
    catalog.touch_keys.clear();
    catalog.edge_keys.clear();

    let copied_origins;
    {
        let valid_sessions =
            session_catalog(&catalog.sources, &catalog.sessions, &mut catalog.summary);
        catalog
            .sessions
            .retain(|key, _| valid_sessions.contains(key));

        let mut invalid_events = Vec::new();
        catalog.events.retain(|key, event| {
            let valid = catalog
                .sessions
                .contains_key(&(key.0.clone(), key.1.clone()));
            if !valid {
                invalid_events.push((
                    event.line_number,
                    format!(
                        "event references unknown session `{}` in source `{}`",
                        key.1, key.0
                    ),
                ));
            }
            valid
        });
        for (line_number, error) in invalid_events {
            push_provider_import_failure(&mut catalog.summary, line_number, error);
        }

        copied_origins = validate_copied_origins(
            catalog.lineage_contract,
            &catalog.sessions,
            &catalog.events,
            &mut catalog.summary,
        );

        let mut valid_touches = Vec::with_capacity(catalog.touches.len());
        for touch in catalog.touches.drain(..) {
            let session_key = (touch.source_id.clone(), touch.session_id.clone());
            let error = if !catalog.sessions.contains_key(&session_key) {
                Some(format!(
                    "file_touch references unknown session `{}` in source `{}`",
                    touch.session_id, touch.source_id
                ))
            } else if let Some(event_index) = touch.event_index {
                let event_key = (
                    touch.source_id.clone(),
                    touch.session_id.clone(),
                    event_index,
                );
                (!catalog.events.contains_key(&event_key))
                    .then(|| format!("file_touch references unknown event_index `{event_index}`"))
            } else {
                None
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, touch.line_number, error);
            } else {
                valid_touches.push(touch);
            }
        }

        let mut valid_edges = Vec::with_capacity(catalog.edges.len());
        for edge in catalog.edges.drain(..) {
            let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
            let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
            let error = if !catalog.sessions.contains_key(&from_key) {
                Some(format!(
                    "edge references unknown from_session_id `{}`",
                    edge.from_session_id
                ))
            } else if !catalog.sessions.contains_key(&to_key) {
                Some(format!(
                    "edge references unknown to_session_id `{}`",
                    edge.to_session_id
                ))
            } else if edge.edge_type == SessionEdgeType::ParentChild {
                catalog.sessions.get(&to_key).and_then(|child| {
                    child.parent_session_id.as_ref().and_then(|parent| {
                        (parent != &edge.from_session_id).then(|| {
                            format!(
                                "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                                edge.from_session_id
                            )
                        })
                    })
                })
            } else {
                None
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, edge.line_number, error);
            } else {
                valid_edges.push(edge);
            }
        }

        let required = catalog
            .events
            .keys()
            .map(|key| (key.0.clone(), key.1.clone()))
            .chain(
                valid_touches
                    .iter()
                    .map(|touch| (touch.source_id.clone(), touch.session_id.clone())),
            )
            .chain(valid_edges.iter().flat_map(|edge| {
                [
                    (edge.source_id.clone(), edge.from_session_id.clone()),
                    (edge.source_id.clone(), edge.to_session_id.clone()),
                ]
            }))
            .collect::<BTreeSet<_>>();
        catalog.sessions.retain(|key, _| required.contains(key));
    }

    let mut rejected_lines = catalog
        .summary
        .failures
        .iter()
        .filter_map(|failure| (failure.line != 0).then_some(failure.line))
        .collect::<BTreeSet<_>>();
    rejected_lines.extend(catalog.oversized_lines);
    let retained_lines = catalog
        .events
        .values()
        .map(|event| event.line_number)
        .collect::<BTreeSet<_>>();
    let complete_records = u64::try_from(complete_records)
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records = u64::try_from(catalog.events.len())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records_before_prior_prefix = prior_prefix_bytes
        .map(|prior_prefix_bytes| {
            u64::try_from(
                catalog
                    .events
                    .values()
                    .filter(|event| event.line.byte_offset < prior_prefix_bytes)
                    .count(),
            )
            .map_err(|_| CustomHistorySourceBackedError::CountMismatch)
        })
        .transpose()?;
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.retained_events_before_prior_prefix = retained_records_before_prior_prefix
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0);
    });
    let rejected_records = u64::try_from(
        rejected_lines
            .iter()
            .filter(|line| **line <= complete_records as usize && !retained_lines.contains(*line))
            .count(),
    )
    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|value| value.checked_sub(rejected_records))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes: certified_prefix_bytes,
    };
    Ok(ParsedProjection {
        sources: catalog.sources,
        sessions: catalog.sessions,
        events: catalog.events,
        copied_origins,
        event_spool,
        observed_prior_prefix_digest,
        retained_records_before_prior_prefix,
        counts,
        checkpoint: CustomHistoryCheckpoint {
            version: CUSTOM_CHECKPOINT_VERSION,
            certified_prefix_bytes,
            complete_records,
            terminal,
        },
        content_digest,
    })
}

fn apply_session_lineage_contract(catalog: &mut ProjectionCatalog) {
    if catalog.lineage_contract.is_none() {
        for session in catalog.sessions.values_mut() {
            session.session_relationship = None;
        }
        for event in catalog.events.values_mut() {
            event.copied_from = None;
        }
        return;
    }

    for session in catalog.sessions.values_mut() {
        let Some(kind) = session.session_relationship else {
            continue;
        };
        let valid = match kind {
            SessionRelationshipKind::Root => {
                session.parent_session_id.is_none()
                    && session
                        .root_session_id
                        .as_deref()
                        .is_none_or(|root| root == session.session_id)
            }
            SessionRelationshipKind::Delegated
            | SessionRelationshipKind::Forked
            | SessionRelationshipKind::ResumedFrom
            | SessionRelationshipKind::WorkflowChild
            | SessionRelationshipKind::RelatedUnknown => {
                session.parent_session_id.as_deref().is_some_and(|parent| {
                    parent != session.session_id
                        && session
                            .root_session_id
                            .as_deref()
                            .is_none_or(|root| root == parent)
                })
            }
        };
        if !valid {
            push_provider_import_failure(
                &mut catalog.summary,
                session.line_number,
                "session_relationship conflicts with parent_session_id/root_session_id".to_owned(),
            );
            session.session_relationship = None;
        }
    }
}

fn validate_copied_origins(
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    sessions: &BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    events: &BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    summary: &mut ProviderImportSummary,
) -> BTreeMap<CustomEventKey, ValidatedCopiedFrom> {
    if lineage_contract.is_none() {
        return BTreeMap::new();
    }

    let mut native_sessions = BTreeMap::<(String, String), Option<String>>::new();
    for session in sessions.values() {
        let Some(native_session_id) = session.native_session_id.as_ref() else {
            continue;
        };
        if !stable_lineage_identifier(native_session_id) {
            continue;
        }
        let entry = native_sessions
            .entry((session.source_id.clone(), native_session_id.clone()))
            .or_insert_with(|| Some(session.session_id.clone()));
        if entry.as_deref() != Some(&session.session_id) {
            *entry = None;
        }
    }

    let mut native_events = BTreeMap::<(String, String, String), Option<(u64, usize)>>::new();
    for (key, event) in events {
        let Some(event_id) = event.event_id.as_ref() else {
            continue;
        };
        if !stable_lineage_identifier(event_id) {
            continue;
        }
        let entry = native_events
            .entry((key.0.clone(), key.1.clone(), event_id.clone()))
            .or_insert(Some((key.2, event.line_number)));
        if entry.is_some_and(|(event_index, _)| event_index != key.2) {
            *entry = None;
        }
    }

    let mut admitted = BTreeMap::new();
    for (key, event) in events {
        let Some(selector) = event.copied_from.as_ref() else {
            continue;
        };
        if !stable_lineage_identifier(&selector.ancestor_native_session_id)
            || !stable_lineage_identifier(&selector.ancestor_event_id)
        {
            push_provider_import_failure(
                summary,
                event.line_number,
                "copied_from native selectors must be non-empty bounded identifiers".to_owned(),
            );
            continue;
        }
        let child_session_key = (key.0.clone(), key.1.clone());
        let child_session = sessions.get(&child_session_key);
        let child_native_session_is_exact = child_session
            .and_then(|session| session.native_session_id.as_ref())
            .and_then(|native_session_id| {
                native_sessions.get(&(key.0.clone(), native_session_id.clone()))
            })
            .and_then(Option::as_deref)
            == Some(key.1.as_str());
        let child_event_is_exact = event.event_id.as_ref().is_some_and(|event_id| {
            native_events
                .get(&(key.0.clone(), key.1.clone(), event_id.clone()))
                .and_then(|entry| *entry)
                .is_some_and(|(event_index, _)| event_index == key.2)
        });
        let direct_parent_session_id = child_session.and_then(|session| {
            matches!(
                session.session_relationship,
                Some(
                    SessionRelationshipKind::Delegated
                        | SessionRelationshipKind::Forked
                        | SessionRelationshipKind::ResumedFrom
                        | SessionRelationshipKind::WorkflowChild
                        | SessionRelationshipKind::RelatedUnknown
                )
            )
            .then_some(session.parent_session_id.as_deref())
            .flatten()
        });
        let proof_identity_is_exact = !matches!(
            selector.proof,
            CtxHistoryJsonlCopyProofKind::NativeEventIdentity
        ) || event.event_id.as_deref()
            == Some(selector.ancestor_event_id.as_str());

        if !child_native_session_is_exact
            || !child_event_is_exact
            || direct_parent_session_id.is_none()
            || !proof_identity_is_exact
        {
            push_provider_import_failure(
                summary,
                event.line_number,
                "copied_from requires unique stable child native session/event IDs, a direct typed parent relationship, and proof-consistent identity"
                    .to_owned(),
            );
            continue;
        }
        // The typed relationship and copied selector are child-owned proof.
        // Derive the durable unresolved IDs from them without consulting the
        // mutable target catalog; target presence is resolution state, not
        // claim validity.
        let Some(ancestor_session_id) = direct_parent_session_id else {
            continue;
        };
        let proof = match selector.proof {
            CtxHistoryJsonlCopyProofKind::NativeEventIdentity => {
                EventCopyProofKind::NativeEventIdentity
            }
            CtxHistoryJsonlCopyProofKind::NativeCopiedFromField => {
                EventCopyProofKind::NativeCopiedFromField
            }
            CtxHistoryJsonlCopyProofKind::NativeCallResultIdentity => {
                EventCopyProofKind::NativeCallResultIdentity
            }
        };
        admitted.insert(
            key.clone(),
            ValidatedCopiedFrom {
                ancestor_session_id: ancestor_session_id.to_owned(),
                ancestor_event_id: selector.ancestor_event_id.clone(),
                proof,
            },
        );
    }
    admitted
}

fn stable_lineage_identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES
        && !value.chars().any(char::is_control)
}

fn session_catalog(
    sources: &BTreeMap<String, CustomSourceCatalogEntry>,
    sessions: &BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    summary: &mut ProviderImportSummary,
) -> BTreeSet<CustomSessionKey> {
    let mut valid = BTreeSet::new();
    for (key, session) in sessions {
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.session_nodes = work.session_nodes.saturating_add(1);
        });
        let source_exists = sources.contains_key(&key.0);
        let has_self_parent = session.parent_session_id.as_deref() == Some(&session.session_id);
        if source_exists && !has_self_parent {
            valid.insert(key.clone());
            continue;
        }
        let detail = if has_self_parent {
            "declares itself as its direct parent"
        } else {
            "references an unknown source"
        };
        push_provider_import_failure(
            summary,
            session.line_number,
            format!("session `{}` in source `{}` {detail}", key.1, key.0,),
        );
    }
    valid
}

fn new_prefix_hasher() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest
}

fn new_complete_prefix_hasher() -> JsonlResumableSha256 {
    let mut digest = JsonlResumableSha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest
}

fn finish_complete_prefix_digest(hasher: &JsonlResumableSha256, prefix_bytes: u64) -> [u8; 32] {
    let mut digest = hasher.clone();
    digest.update(&prefix_bytes.to_be_bytes());
    digest.digest()
}

fn finish_prefix_digest(hasher: &Sha256, prefix_bytes: u64) -> [u8; 32] {
    let mut digest = hasher.clone();
    digest.update(prefix_bytes.to_be_bytes());
    digest.finalize().into()
}
