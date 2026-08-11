//! Delta-only Hermes route projection and profile-owned source decoding.

use super::*;

pub(super) fn observe_hermes_incremental_inventory(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    prior: &HermesRefreshReceipt,
    pinned_session_rowid: i64,
    pinned_message_rowid: i64,
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    context: &mut dyn HermesReconciliationContext,
) -> HermesSourceBackedResult<HermesSessionInventory> {
    context.report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    let mut new_sessions = BTreeMap::<String, i64>::new();
    let mut observed_rows = 0_u64;
    let mut after_session_rowid = prior.session_rowid;
    loop {
        let page =
            hermes_session_identity_page(conn, Some(after_session_rowid), pinned_session_rowid)?;
        if page.is_empty() {
            break;
        }
        for row in page {
            validate_session_key(&row.provider_session_id)?;
            after_session_rowid = row.rowid;
            observed_rows = checked_add(observed_rows, 1)?;
            if new_sessions
                .insert(row.provider_session_id.clone(), row.rowid)
                .is_some()
            {
                return Err(CaptureError::InvalidPayload(format!(
                    "Hermes session {} is duplicated",
                    row.provider_session_id
                ))
                .into());
            }
        }
        context.report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }

    let mut touched = new_sessions.keys().cloned().collect::<BTreeSet<_>>();
    let mut message_sessions = BTreeMap::<i64, String>::new();
    let mut after_message_rowid = prior.message_rowid;
    loop {
        let page = hermes_message_cursor_page(conn, after_message_rowid, pinned_message_rowid)?;
        if page.is_empty() {
            break;
        }
        for row in page {
            validate_session_key(&row.provider_session_id)?;
            after_message_rowid = row.rowid;
            observed_rows = checked_add(observed_rows, 1)?;
            touched.insert(row.provider_session_id.clone());
            message_sessions.insert(row.rowid, row.provider_session_id);
        }
        context.report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }

    let mut messages = BTreeMap::<String, Vec<HermesNativeRow>>::new();
    let mut reader = HermesRowReader::new(conn, &schema)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut frontier = super::super::sqlite::HermesFrontier {
        phase: HermesPhase::Messages,
        next_ordinal: 0,
        rowid: prior.message_rowid,
    };
    loop {
        let page = reader
            .next_page(frontier)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if page.is_empty() {
            break;
        }
        frontier = page.last().map(|row| row.next_frontier).unwrap_or(frontier);
        for native in page {
            if native.locator.rowid > pinned_message_rowid {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            let provider_session_id = match &native.record {
                HermesNativeRecord::Message { row, .. } => row.session_id.clone(),
                HermesNativeRecord::Rejected(_) => message_sessions
                    .get(&native.locator.rowid)
                    .cloned()
                    .ok_or(CaptureError::SourceChangedDuringCapture)?,
                HermesNativeRecord::Session(_) => {
                    return Err(CaptureError::SystemInvariant(
                        "Hermes incremental message traversal returned a session row",
                    )
                    .into())
                }
            };
            messages
                .entry(provider_session_id)
                .or_default()
                .push(native);
        }
    }

    let mut leaves = Vec::with_capacity(touched.len());
    for provider_session_id in touched {
        let source = hermes_session_source_key(&candidate.source, &provider_session_id)?;
        let base = context.exact_base_source(&source).filter(|base| {
            base.observation().source().exact_descriptor_eq(&source)
                && hermes_provider_session_id(&candidate.source, base.observation().source())
                    .as_deref()
                    == Some(provider_session_id.as_str())
        });
        if base.is_none() && !new_sessions.contains_key(&provider_session_id) {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let mut session_reader = HermesRowReader::for_session(conn, &schema, &provider_session_id)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        let mut session_rows = session_reader
            .next_session_inventory_page(None)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if session_rows.len() != 1 {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let mut session = session_rows.remove(0);
        let mut delta_messages = messages.remove(&provider_session_id).unwrap_or_default();
        if base.is_some() && delta_messages.is_empty() {
            continue;
        }
        let mut next_ordinal = base
            .as_ref()
            .map_or(0, |base| base.counts().complete_records);
        if base.is_none() {
            session.ordinal = 0;
            session.next_frontier.next_ordinal = 1;
            next_ordinal = 1;
        }
        for message in &mut delta_messages {
            message.ordinal = next_ordinal;
            next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or(HermesSourceBackedError::CountOverflow)?;
            message.next_frontier.next_ordinal = next_ordinal;
        }
        let leaf_message_rowid = delta_messages
            .last()
            .map_or(prior.message_rowid, |message| message.locator.rowid);
        let (observation_revision, fingerprint) = incremental_session_fingerprint(
            &source,
            &provider_session_id,
            &schema_evidence,
            new_sessions
                .get(&provider_session_id)
                .copied()
                .unwrap_or_default(),
            leaf_message_rowid,
        );
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            HermesSessionLeaf {
                provider_session_id,
                source,
                observation_revision,
                incremental: Some(HermesIncrementalLeaf {
                    base,
                    session,
                    messages: delta_messages,
                }),
            },
        ));
    }
    record_inventory_rows(observed_rows);
    let tree_fingerprint = hermes_tree_fingerprint(&candidate.source, &schema_evidence, &leaves);
    Ok(HermesSessionInventory {
        schema,
        schema_evidence,
        leaves,
        tree_fingerprint,
        max_session_rowid: after_session_rowid,
        max_message_rowid: after_message_rowid,
        reconciliation_demand: SourceBackedReconciliationDemand::Incremental,
        publication_receipt: None,
    })
}

pub(super) fn hermes_provider_session_id(
    profile_source: &SourceKey,
    source: &SourceKey,
) -> Option<String> {
    if source.provider() != CaptureProvider::Hermes.as_str()
        || source.source_format() != HERMES_SQLITE_SOURCE_FORMAT
        || source.schema_variant() != HERMES_SESSION_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
    {
        return None;
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return None;
    };
    if namespace != HERMES_SESSION_SOURCE_ANCHOR_NAMESPACE {
        return None;
    }
    let TypedKey::Composite(parts) = key else {
        return None;
    };
    let [TypedKey::Bytes(profile), TypedKey::Utf8(session)] = parts.as_slice() else {
        return None;
    };
    let expected = profile_source.identity().encode_canonical().ok()?;
    (profile.as_slice() == expected.as_slice()).then(|| session.clone())
}

fn incremental_session_fingerprint(
    source: &SourceKey,
    provider_session_id: &str,
    schema_evidence: &[u8],
    session_rowid: i64,
    message_rowid: i64,
) -> (Vec<u8>, DocumentLeafFingerprint) {
    let mut digest = Sha256::new();
    digest.update(HERMES_INCREMENTAL_FINGERPRINT_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    hash_text(&mut digest, provider_session_id);
    hash_bytes(&mut digest, schema_evidence);
    digest.update(session_rowid.to_be_bytes());
    digest.update(message_rowid.to_be_bytes());
    let revision: [u8; 32] = digest.finalize().into();
    let mut fingerprint = Sha256::new();
    fingerprint.update(HERMES_LEAF_FINGERPRINT_DOMAIN);
    fingerprint.update(source.exact_descriptor_digest());
    fingerprint.update(revision);
    (
        revision.to_vec(),
        DocumentLeafFingerprint::new(fingerprint.finalize().into()),
    )
}
