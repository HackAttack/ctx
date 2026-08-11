//! Provider-local source-backed Hermes adapter.
//!
//! This module deliberately stops at discovery, bounded native projection,
//! source certification, and complete direct Core projection. Publication,
//! replacement/deletion lifecycle, and projection fanout remain shared
//! responsibilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SessionRelationshipKind, SourceAnchor, SourceKey,
    SourceObservation, StableEntityId, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
        normalization::provider_required_timestamp_seconds,
        source_backed::{
            family::document::{
                document_frontier_fingerprint, DocumentLeafFingerprint, ObservedDocumentLeaf,
            },
            SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
            SourceBackedReconciliationDemand, SourceBackedRouteError, SourceBackedRouteResult,
        },
        sqlite::sqlite_schema_fingerprint,
    },
    provider_sources::{
        retain_sqlite_source_directory_authority, ProviderSource, SqliteArtifactKind,
        SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
        SqliteSourceDirectoryAuthority, SqliteSourceErrorComposition, SqliteSourceEvidence,
        SqliteSourceProgressError, SqliteSourceReadSnapshot,
    },
    CaptureError, HERMES_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    hermes_layout_record_digest, hermes_native_event,
    layout::{HermesMessageRow, HermesSchema, HermesSessionRow},
    sqlite::{
        hermes_max_rowid, hermes_message_cursor_page, hermes_message_session_id,
        hermes_session_identity_page, HermesNativeRecord, HermesNativeRow, HermesPhase,
        HermesRowReader,
    },
    HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

const HERMES_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile";
const HERMES_SESSION_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile-session";
const HERMES_CONTROL_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile-control";
const HERMES_SESSION_NAMESPACE: &str = "hermes.session";
const HERMES_MESSAGE_NAMESPACE: &str = "hermes.message";
const HERMES_LOGICAL_SESSION_KIND: &str = "hermes-session";
const HERMES_LOGICAL_EVENT_KIND: &str = "hermes-message";
const HERMES_PROFILE_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-db-v1";
const HERMES_SESSION_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-session-v1";
const HERMES_CONTROL_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-control-v1";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Hermes SQLite source must have an authorized parent and database leaf";
const HERMES_SOURCE_PARSER_REVISION: &str = "hermes-source-backed-v3";
const HERMES_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-session-content-v1\0";
const HERMES_TREE_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-source-inventory-v1\0";
const HERMES_LEAF_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-session-leaf-v1\0";
const HERMES_SESSION_OBSERVATION_DOMAIN: &[u8] = b"ctx-hermes-session-observation-v1\0";
const HERMES_MESSAGE_OBSERVATION_DOMAIN: &[u8] = b"ctx-hermes-message-observation-v1\0";
const HERMES_SESSION_OBSERVATION_KIND: &str = "hermes-session-observation-v1";
const HERMES_CONTROL_OBSERVATION_KIND: &str = "hermes-refresh-control-v1";
const HERMES_CONTROL_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-refresh-control-v1\0";
const HERMES_INCREMENTAL_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-incremental-session-v1\0";
const HERMES_EXACT_INTERVAL_MS: i64 = 60 * 60 * 1_000;
const HERMES_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-session-v1\0";
const HERMES_REJECTION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-rejection-v1\0";

mod contracts;
mod replacement;

pub(crate) use contracts::*;

const HERMES_SESSION_KEY_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct HermesSessionContext {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    branch: Option<String>,
    agent_type: String,
    is_primary: bool,
    workspace: Option<String>,
    cwd: Option<String>,
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
    let is_primary = parent_session_id.is_none();
    let root_session_id = parent_session_id.unwrap_or(session_id);

    Ok(HermesSessionContext {
        session_id,
        parent_session_id,
        root_session_id,
        branch: row.git_branch.clone(),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary,
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

fn validate_session_key(value: &str) -> Result<(), CaptureError> {
    if value.len() > HERMES_SESSION_KEY_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "Hermes session identifier exceeds the {HERMES_SESSION_KEY_MAX_BYTES}-byte Core key bound"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSessionLeaf {
    provider_session_id: String,
    source: SourceKey,
    observation_revision: Vec<u8>,
    control_receipt: Option<HermesRefreshReceipt>,
}

struct HermesSessionInventory {
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    leaves: Vec<ObservedDocumentLeaf<HermesSessionLeaf>>,
    tree_fingerprint: [u8; 32],
    max_session_rowid: i64,
    max_message_rowid: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct HermesRefreshReceipt {
    version: u32,
    database_identity: [u8; 32],
    schema_evidence: [u8; 32],
    session_rowid: i64,
    message_rowid: i64,
    last_successful_exhaustive_at_ms: i64,
    exact_due_at_ms: i64,
    exhaustive_sequence: u64,
    mode: String,
    outcome: String,
}

struct HermesSessionObservationBuilder {
    session_row: Option<(i64, [u8; 32])>,
    message_count: u64,
    message_keys: Sha256,
}

impl HermesSessionObservationBuilder {
    fn new() -> Self {
        let mut message_keys = Sha256::new();
        message_keys.update(HERMES_MESSAGE_OBSERVATION_DOMAIN);
        Self {
            session_row: None,
            message_count: 0,
            message_keys,
        }
    }

    fn record_session(
        &mut self,
        rowid: i64,
        row: &HermesSessionRow,
    ) -> HermesSourceBackedResult<()> {
        if self
            .session_row
            .replace((rowid, session_record_digest(row)))
            .is_some()
        {
            return Err(HermesSourceBackedError::Capture(
                CaptureError::InvalidPayload(format!("Hermes session {} is duplicated", row.id)),
            ));
        }
        Ok(())
    }

    fn record_message(
        &mut self,
        rowid: i64,
        record_digest: [u8; 32],
    ) -> HermesSourceBackedResult<()> {
        self.message_count = checked_add(self.message_count, 1)?;
        self.message_keys.update(rowid.to_be_bytes());
        self.message_keys.update(record_digest);
        Ok(())
    }

    fn finish(
        self,
        source: &SourceKey,
        schema_evidence: &[u8],
    ) -> ([u8; 32], DocumentLeafFingerprint) {
        let mut revision = Sha256::new();
        revision.update(HERMES_SESSION_OBSERVATION_DOMAIN);
        hash_bytes(&mut revision, schema_evidence);
        revision.update(source.exact_descriptor_digest());
        match self.session_row {
            Some((rowid, digest)) => {
                revision.update([1]);
                revision.update(rowid.to_be_bytes());
                revision.update(digest);
            }
            None => revision.update([0]),
        }
        revision.update(self.message_count.to_be_bytes());
        revision.update(self.message_keys.finalize());
        let revision: [u8; 32] = revision.finalize().into();

        let mut fingerprint = Sha256::new();
        fingerprint.update(HERMES_LEAF_FINGERPRINT_DOMAIN);
        fingerprint.update(source.exact_descriptor_digest());
        fingerprint.update(revision);
        (
            revision,
            DocumentLeafFingerprint::new(fingerprint.finalize().into()),
        )
    }
}

#[cfg(test)]
fn observe_hermes_session_inventory(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory> {
    report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    let (schema, schema_evidence) = detect_hermes_schema(conn)?;

    observe_hermes_session_inventory_with_schema(
        candidate,
        conn,
        schema,
        schema_evidence,
        report_progress,
    )
}

fn detect_hermes_schema(
    conn: &rusqlite::Connection,
) -> HermesSourceBackedResult<(HermesSchema, Vec<u8>)> {
    let sqlite_user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    let schema_fingerprint = sqlite_schema_fingerprint(conn)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    let schema = HermesSchema::detect(conn)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Schema))?;
    let schema_evidence = hermes_schema_evidence(sqlite_user_version, &schema_fingerprint);
    Ok((schema, schema_evidence))
}

fn observe_hermes_session_inventory_with_schema(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory> {
    let mut builders = BTreeMap::<String, HermesSessionObservationBuilder>::new();
    let mut observed_rows = 0_u64;
    let mut session_reader = HermesRowReader::new(conn, &schema)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut after_session_rowid = None;
    loop {
        let page = session_reader
            .next_session_inventory_page(after_session_rowid)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if page.is_empty() {
            break;
        }
        for native in page {
            after_session_rowid = Some(native.locator.rowid);
            observed_rows = checked_add(observed_rows, 1)?;
            match native.record {
                HermesNativeRecord::Session(row) => {
                    hermes_session_source_key(&candidate.source, &row.id)?;
                    builders
                        .entry(row.id.clone())
                        .or_insert_with(HermesSessionObservationBuilder::new)
                        .record_session(native.locator.rowid, &row)?;
                }
                HermesNativeRecord::Rejected(reason) => {
                    return Err(HermesSourceBackedError::Capture(CaptureError::InvalidPayload(
                        format!("Hermes cannot derive a logical source for rejected session row: {reason}"),
                    )));
                }
                HermesNativeRecord::Message { .. } => {
                    return Err(HermesSourceBackedError::Capture(
                        CaptureError::SystemInvariant(
                            "Hermes session inventory crossed into message rows",
                        ),
                    ));
                }
            }
        }
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }
    drop(session_reader);

    let mut after_message_rowid = None;
    let mut message_reader = HermesRowReader::new(conn, &schema)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut frontier = super::sqlite::HermesFrontier::initial();
    loop {
        let page = message_reader
            .next_page(frontier)
            .map_err(HermesSourceBackedError::from)
            .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
        if page.is_empty() {
            break;
        }
        frontier = page
            .last()
            .map(|native| native.next_frontier)
            .unwrap_or(frontier);
        for native in page {
            if native.locator.phase == HermesPhase::Sessions {
                continue;
            }
            after_message_rowid = Some(native.locator.rowid);
            observed_rows = checked_add(observed_rows, 1)?;
            let record_digest = native_record_digest(&native)?;
            let provider_session_id = match &native.record {
                HermesNativeRecord::Message { row, .. } => row.session_id.clone(),
                HermesNativeRecord::Rejected(_) => {
                    hermes_message_session_id(conn, native.locator.rowid)?
                }
                HermesNativeRecord::Session(_) => {
                    return Err(CaptureError::SystemInvariant(
                        "Hermes message inventory returned a session row",
                    )
                    .into())
                }
            };
            hermes_session_source_key(&candidate.source, &provider_session_id)?;
            builders
                .entry(provider_session_id)
                .or_insert_with(HermesSessionObservationBuilder::new)
                .record_message(native.locator.rowid, record_digest)?;
        }
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }

    let mut leaves = Vec::with_capacity(builders.len());
    for (provider_session_id, builder) in builders {
        let source = hermes_session_source_key(&candidate.source, &provider_session_id)?;
        let (observation_revision, fingerprint) = builder.finish(&source, &schema_evidence);
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            HermesSessionLeaf {
                provider_session_id,
                source,
                observation_revision: observation_revision.to_vec(),
                control_receipt: None,
            },
        ));
    }
    let tree_fingerprint = hermes_tree_fingerprint(&candidate.source, &schema_evidence, &leaves);
    record_inventory_rows(observed_rows);
    report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        observed_rows,
        0,
    ))?;
    Ok(HermesSessionInventory {
        schema,
        schema_evidence,
        leaves,
        tree_fingerprint,
        max_session_rowid: after_session_rowid.unwrap_or(0),
        max_message_rowid: after_message_rowid.unwrap_or(0),
    })
}

fn observe_hermes_reconciliation_inventory(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    base_sources: &[CertifiedSource],
    requested: SourceBackedReconciliationDemand,
    database_identity: [u8; 32],
    now_ms: i64,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory> {
    let (schema, schema_evidence) = detect_hermes_schema(conn)?;
    let schema_digest: [u8; 32] = Sha256::digest(&schema_evidence).into();
    let prior = hermes_refresh_receipt(candidate, base_sources)?;
    let current_session_rowid = hermes_max_rowid(conn, "sessions")?;
    let current_message_rowid = hermes_max_rowid(conn, "messages")?;
    let forced_exhaustive = prior.as_ref().is_none_or(|receipt| {
        receipt.version != 1
            || receipt.database_identity != database_identity
            || receipt.schema_evidence != schema_digest
            || current_session_rowid < receipt.session_rowid
            || current_message_rowid < receipt.message_rowid
    });
    let demand = if requested == SourceBackedReconciliationDemand::Exhaustive || forced_exhaustive {
        SourceBackedReconciliationDemand::Exhaustive
    } else {
        SourceBackedReconciliationDemand::Incremental
    };

    let mut inventory = if demand == SourceBackedReconciliationDemand::Exhaustive {
        observe_hermes_session_inventory_with_schema(
            candidate,
            conn,
            schema,
            schema_evidence.clone(),
            report_progress,
        )?
    } else {
        observe_hermes_incremental_inventory(
            candidate,
            conn,
            base_sources,
            prior.as_ref().expect("incremental Hermes receipt"),
            schema,
            schema_evidence.clone(),
            report_progress,
        )?
    };

    let (last_exact, exact_due, exhaustive_sequence) = match demand {
        SourceBackedReconciliationDemand::Exhaustive => (
            now_ms,
            now_ms.saturating_add(HERMES_EXACT_INTERVAL_MS),
            prior.as_ref().map_or(1, |receipt| {
                if receipt.last_successful_exhaustive_at_ms == now_ms {
                    receipt.exhaustive_sequence
                } else {
                    receipt.exhaustive_sequence.saturating_add(1)
                }
            }),
        ),
        SourceBackedReconciliationDemand::Incremental => {
            let prior = prior.as_ref().expect("incremental Hermes receipt");
            (
                prior.last_successful_exhaustive_at_ms,
                prior.exact_due_at_ms,
                prior.exhaustive_sequence,
            )
        }
    };
    let mode = match (demand, prior.as_ref()) {
        (SourceBackedReconciliationDemand::Incremental, Some(prior))
            if prior.session_rowid == inventory.max_session_rowid
                && prior.message_rowid == inventory.max_message_rowid =>
        {
            prior.mode.clone()
        }
        _ => demand.as_str().to_owned(),
    };
    let receipt = HermesRefreshReceipt {
        version: 1,
        database_identity,
        schema_evidence: schema_digest,
        session_rowid: inventory.max_session_rowid,
        message_rowid: inventory.max_message_rowid,
        last_successful_exhaustive_at_ms: last_exact,
        exact_due_at_ms: exact_due,
        exhaustive_sequence,
        mode,
        outcome: "successful".to_owned(),
    };
    inventory
        .leaves
        .push(hermes_control_leaf(candidate, receipt)?);
    inventory.tree_fingerprint =
        hermes_tree_fingerprint(&candidate.source, &schema_evidence, &inventory.leaves);
    Ok(inventory)
}

fn observe_hermes_incremental_inventory(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    base_sources: &[CertifiedSource],
    prior: &HermesRefreshReceipt,
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<HermesSessionInventory> {
    report_progress(hermes_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    let mut live = BTreeMap::<String, i64>::new();
    let mut observed_rows = 0_u64;
    let mut after_session_rowid = None;
    loop {
        let page = hermes_session_identity_page(conn, after_session_rowid)?;
        if page.is_empty() {
            break;
        }
        for row in page {
            validate_session_key(&row.provider_session_id)?;
            after_session_rowid = Some(row.rowid);
            observed_rows = checked_add(observed_rows, 1)?;
            if live
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
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }

    let base_sessions = hermes_base_sessions(candidate, base_sources)?;
    let mut touched = live
        .iter()
        .filter(|(session_id, rowid)| {
            **rowid > prior.session_rowid || !base_sessions.contains_key(*session_id)
        })
        .map(|(session_id, _)| session_id.clone())
        .collect::<BTreeSet<_>>();
    let mut after_message_rowid = prior.message_rowid;
    loop {
        let page = hermes_message_cursor_page(conn, after_message_rowid)?;
        if page.is_empty() {
            break;
        }
        for row in page {
            validate_session_key(&row.provider_session_id)?;
            after_message_rowid = row.rowid;
            observed_rows = checked_add(observed_rows, 1)?;
            touched.insert(row.provider_session_id);
        }
        report_progress(hermes_logical_progress(
            SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
            observed_rows,
            0,
        ))?;
    }

    let mut session_ids = live.keys().cloned().collect::<BTreeSet<_>>();
    session_ids.extend(base_sessions.keys().cloned());
    let mut leaves = Vec::with_capacity(session_ids.len());
    for provider_session_id in session_ids {
        let source = hermes_session_source_key(&candidate.source, &provider_session_id)?;
        let (observation_revision, fingerprint) = if !touched.contains(&provider_session_id) {
            if let Some((base, fingerprint)) = base_sessions.get(&provider_session_id) {
                (base.observation().revision().to_vec(), *fingerprint)
            } else {
                incremental_session_fingerprint(
                    &source,
                    &provider_session_id,
                    &schema_evidence,
                    live.get(&provider_session_id).copied().unwrap_or_default(),
                    after_message_rowid,
                )
            }
        } else {
            incremental_session_fingerprint(
                &source,
                &provider_session_id,
                &schema_evidence,
                live.get(&provider_session_id).copied().unwrap_or_default(),
                after_message_rowid,
            )
        };
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            HermesSessionLeaf {
                provider_session_id,
                source,
                observation_revision,
                control_receipt: None,
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
        max_session_rowid: after_session_rowid.unwrap_or(0),
        max_message_rowid: hermes_max_rowid(conn, "messages")?,
    })
}

fn hermes_base_sessions<'a>(
    candidate: &HermesSourceCandidate,
    base_sources: &'a [CertifiedSource],
) -> HermesSourceBackedResult<BTreeMap<String, (&'a CertifiedSource, DocumentLeafFingerprint)>> {
    let mut sessions = BTreeMap::new();
    for base in base_sources {
        let source = base.observation().source();
        if source.schema_variant() != HERMES_SESSION_SOURCE_SCHEMA_VARIANT
            || base.parser_revision() != HERMES_SOURCE_PARSER_REVISION
        {
            continue;
        }
        let Some(provider_session_id) = hermes_provider_session_id(&candidate.source, source)
        else {
            continue;
        };
        let Some(fingerprint) = document_frontier_fingerprint(base) else {
            continue;
        };
        if sessions
            .insert(provider_session_id.clone(), (base, fingerprint))
            .is_some()
        {
            return Err(CaptureError::InvalidPayload(format!(
                "Hermes base generation duplicates session {provider_session_id}"
            ))
            .into());
        }
    }
    Ok(sessions)
}

fn hermes_provider_session_id(profile_source: &SourceKey, source: &SourceKey) -> Option<String> {
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

fn hermes_refresh_receipt(
    candidate: &HermesSourceCandidate,
    base_sources: &[CertifiedSource],
) -> HermesSourceBackedResult<Option<HermesRefreshReceipt>> {
    let control = hermes_control_source_key(&candidate.source)?;
    let mut matched = base_sources
        .iter()
        .filter(|base| base.observation().source().exact_descriptor_eq(&control));
    let Some(base) = matched.next() else {
        return Ok(None);
    };
    if matched.next().is_some()
        || base.parser_revision() != HERMES_SOURCE_PARSER_REVISION
        || base.observation().revision_kind() != HERMES_CONTROL_OBSERVATION_KIND
    {
        return Ok(None);
    }
    Ok(serde_json::from_slice(base.observation().revision()).ok())
}

fn hermes_control_leaf(
    candidate: &HermesSourceCandidate,
    receipt: HermesRefreshReceipt,
) -> HermesSourceBackedResult<ObservedDocumentLeaf<HermesSessionLeaf>> {
    let source = hermes_control_source_key(&candidate.source)?;
    let revision = serde_json::to_vec(&receipt)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(HERMES_CONTROL_FINGERPRINT_DOMAIN);
    fingerprint.update(source.exact_descriptor_digest());
    fingerprint.update(&revision);
    Ok(ObservedDocumentLeaf::new(
        DocumentLeafFingerprint::new(fingerprint.finalize().into()),
        HermesSessionLeaf {
            provider_session_id: String::new(),
            source,
            observation_revision: revision,
            control_receipt: Some(receipt),
        },
    ))
}

fn hermes_control_source_key(profile_source: &SourceKey) -> HermesSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        HERMES_CONTROL_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::bytes(profile_source.identity().encode_canonical()?.to_vec())?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_CONTROL_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn hermes_now_ms() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX);
    now - now.rem_euclid(60_000)
}

pub(crate) fn hermes_sources_require_exact_reconciliation(
    sources: &[CertifiedSource],
    now_ms: i64,
) -> bool {
    let mut has_hermes_state = false;
    let mut has_control = false;
    for source in sources {
        let key = source.observation().source();
        if key.provider() != CaptureProvider::Hermes.as_str()
            || key.source_format() != HERMES_SQLITE_SOURCE_FORMAT
        {
            continue;
        }
        if key.schema_variant() == HERMES_SESSION_SOURCE_SCHEMA_VARIANT {
            has_hermes_state = true;
            continue;
        }
        if key.schema_variant() != HERMES_CONTROL_SOURCE_SCHEMA_VARIANT {
            continue;
        }
        has_control = true;
        if source.parser_revision() != HERMES_SOURCE_PARSER_REVISION
            || source.observation().revision_kind() != HERMES_CONTROL_OBSERVATION_KIND
        {
            return true;
        }
        let Ok(receipt) =
            serde_json::from_slice::<HermesRefreshReceipt>(source.observation().revision())
        else {
            return true;
        };
        if receipt.version != 1
            || receipt.outcome != "successful"
            || receipt.exact_due_at_ms <= now_ms
        {
            return true;
        }
    }
    has_hermes_state && !has_control
}

struct HermesSessionProjection {
    certificate: CertifiedSource,
    decoded_rows: u64,
    emitted_pages: u64,
    peak_buffered_records: u64,
    native_candidate_query_batches: u64,
    native_hydration_query_batches: u64,
    max_native_rows_per_set: u64,
}

enum HermesSnapshotProjectionOutput {
    Page(HermesSourceBackedPage),
    Progress(SourceBackedCurrentSourceProgress),
}

#[cfg(test)]
fn project_hermes_session_snapshot(
    candidate: &HermesSourceCandidate,
    leaf: &HermesSessionLeaf,
    schema: &HermesSchema,
    conn: &rusqlite::Connection,
    emit: &mut dyn FnMut(HermesSourceBackedPage) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSessionProjection> {
    project_hermes_session_snapshot_with_progress(candidate, leaf, schema, conn, &mut |output| {
        match output {
            HermesSnapshotProjectionOutput::Page(page) => emit(page),
            HermesSnapshotProjectionOutput::Progress(_) => Ok(()),
        }
    })
}

fn project_hermes_session_snapshot_with_progress(
    candidate: &HermesSourceCandidate,
    leaf: &HermesSessionLeaf,
    schema: &HermesSchema,
    conn: &rusqlite::Connection,
    emit: &mut dyn FnMut(HermesSnapshotProjectionOutput) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSessionProjection> {
    leaf.source.validate_contract()?;
    let source_path = candidate
        .path
        .to_str()
        .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(candidate.path.clone()))?
        .to_owned();
    let mut reader = HermesRowReader::for_session(conn, schema, &leaf.provider_session_id)
        .map_err(HermesSourceBackedError::from)
        .map_err(|error| diagnose_hermes_query_error(error, SqliteFailurePhase::Projection))?;
    let mut context = None;
    let mut context_rejection = None;
    let operation: HermesSourceBackedResult<(ScannedSourceCounts, [u8; 32], u64, u64, u64)> =
        (|| {
            let mut frontier = super::sqlite::HermesFrontier::initial();
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

            loop {
                let native_page = reader.next_page(frontier)?;
                if native_page.is_empty() {
                    break;
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
    #[cfg(test)]
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

fn diagnose_hermes_query_error(
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

fn checked_add(left: u64, right: u64) -> HermesSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(HermesSourceBackedError::CountOverflow)
}

#[cfg(test)]
fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(data_root, path, || {}, &mut |_| Ok(()))
}

fn open_root_authorized_snapshot_with_progress(
    data_root: &Path,
    path: &Path,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(data_root, path, || {}, report_progress)
}

#[cfg(test)]
fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(
        data_root,
        path,
        after_authorize,
        &mut |_| Ok(()),
    )
}

fn open_root_authorized_snapshot_with_hook_and_progress(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let admission_root = ProviderSourceRoot::open(parent)?;
    let admission_directory = admission_root.directory()?;
    let parent_handle = admission_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot = sqlite_authority
        .open_stable_snapshot_with_progress(database_leaf, |progress| {
            report_progress(progress.into())
        })
        .map_err(|error| match error {
            SqliteSourceProgressError::Source(error) => HermesSourceBackedError::from(error),
            SqliteSourceProgressError::Progress(error) => HermesSourceBackedError::from(error),
            SqliteSourceProgressError::ProgressAndFinalization {
                primary,
                finalization,
            } => HermesSourceBackedError::from(primary)
                .compose_sqlite_source_finalization(finalization),
        })?;
    after_authorize();
    let configure = (|| {
        sqlite_snapshot.revalidate()?;
        let connection = sqlite_snapshot.connection()?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| HermesSourceBackedError::CountOverflow)?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|source| {
                sqlite_snapshot.diagnose_provider_query_error(
                    "setting the private Hermes SQLite busy timeout",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        Ok(())
    })();
    if let Err(error) = configure {
        return Err(abort_hermes_snapshot(sqlite_snapshot, error));
    }
    Ok((sqlite_authority, sqlite_snapshot))
}

fn abort_hermes_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: HermesSourceBackedError,
) -> HermesSourceBackedError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => HermesSourceBackedError::Route(
            crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
                replacement::hermes_route_error(primary),
                replacement::hermes_sqlite_route_error(cleanup),
            ),
        ),
    }
}

fn hermes_schema_evidence(sqlite_user_version: i64, schema_fingerprint: &str) -> Vec<u8> {
    format!(
        "hermes-logical-schema-v1:capture={HERMES_CAPTURE_REVISION};\
         policy={HERMES_POLICY_REVISION};user_version={sqlite_user_version};\
         schema={schema_fingerprint}",
    )
    .into_bytes()
}

fn hermes_logical_progress(
    stage: SourceBackedCurrentSourceProgressStage,
    rows_scanned: u64,
    certified_bytes: u64,
) -> SourceBackedCurrentSourceProgress {
    let mut progress = SourceBackedCurrentSourceProgress::new(stage);
    progress.logical_rows_scanned = Some(rows_scanned);
    progress.logical_certified_bytes = Some(certified_bytes);
    progress
}

fn hermes_tree_fingerprint(
    profile_source: &SourceKey,
    schema_evidence: &[u8],
    leaves: &[ObservedDocumentLeaf<HermesSessionLeaf>],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_TREE_FINGERPRINT_DOMAIN);
    digest.update(profile_source.exact_descriptor_digest());
    hash_bytes(&mut digest, schema_evidence);
    digest.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        digest.update(leaf.fingerprint.as_bytes());
    }
    digest.finalize().into()
}

#[cfg(test)]
std::thread_local! {
    static HERMES_LOGICAL_ROW_TRAVERSALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HERMES_INVENTORY_ROWS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HERMES_SESSION_SCAN_RECEIPTS: std::cell::RefCell<BTreeMap<String, (u64, u64)>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
pub(crate) fn reset_logical_row_traversals() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| count.set(0));
    HERMES_INVENTORY_ROWS.with(|count| count.set(0));
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| receipts.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn logical_row_traversals() -> u64 {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn inventory_observation_rows() -> u64 {
    HERMES_INVENTORY_ROWS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn session_scan_receipts() -> BTreeMap<String, (u64, u64)> {
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| receipts.borrow().clone())
}

#[cfg(test)]
fn record_logical_row_traversal() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| {
        count.set(count.get().saturating_add(1));
    });
}

#[cfg(test)]
fn record_inventory_rows(rows: u64) {
    HERMES_INVENTORY_ROWS.with(|count| count.set(count.get().saturating_add(rows)));
}

#[cfg(not(test))]
fn record_inventory_rows(_rows: u64) {}

#[cfg(test)]
fn record_session_scan_receipt(
    provider_session_id: &str,
    decoded_rows: u64,
    hydration_queries: u64,
) {
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| {
        receipts.borrow_mut().insert(
            provider_session_id.to_owned(),
            (decoded_rows, hydration_queries),
        );
    });
}

#[cfg(not(test))]
fn record_session_scan_receipt(
    _provider_session_id: &str,
    _decoded_rows: u64,
    _hydration_queries: u64,
) {
}

fn project_native_row(
    source: &SourceKey,
    source_path: &str,
    native: HermesNativeRow,
    session_context: Option<&HermesSessionContext>,
    context_rejection: Option<&str>,
) -> HermesSourceBackedResult<HermesSourceBackedRecord> {
    let ordinal = native.ordinal;
    match native.record {
        HermesNativeRecord::Session(row) => {
            if let Some(reason) = context_rejection {
                return Ok(rejected(reason.to_owned()));
            }
            let Some(context) = session_context else {
                return Ok(rejected(format!(
                    "Hermes session {} disappeared during projection",
                    row.id
                )));
            };
            match project_session(source_path, row, context) {
                Ok(session) => Ok(HermesSourceBackedRecord::Session(session)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Message {
            row,
            values: _,
            prepared,
        } => {
            if let Some(reason) = context_rejection {
                return Ok(rejected(reason.to_owned()));
            }
            let Some(context) = session_context else {
                return Ok(rejected(format!(
                    "Hermes message {} depends on missing session {}",
                    row.id, row.session_id
                )));
            };
            match project_message(source, ordinal, row, prepared, context) {
                Ok(document) => Ok(HermesSourceBackedRecord::Event(document)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Rejected(reason) => Ok(rejected(reason)),
    }
}

fn rejected(reason: String) -> HermesSourceBackedRecord {
    HermesSourceBackedRecord::Rejected(HermesSourceBackedRejection { reason })
}

fn project_session(
    source_path: &str,
    row: HermesSessionRow,
    context: &HermesSessionContext,
) -> HermesSourceBackedResult<HermesSourceBackedSession> {
    Ok(HermesSourceBackedSession {
        provider_session_id: row.id,
        provider_parent_session_id: row.parent_session_id,
        branch: context.branch.clone(),
        source_path: source_path.to_owned(),
        agent_type: context.agent_type.clone(),
        workspace: context.workspace.clone(),
        cwd: context.cwd.clone(),
    })
}

fn project_message(
    source: &SourceKey,
    ordinal: u64,
    row: HermesMessageRow,
    prepared: Option<super::HermesPreparedCoreMessage>,
    session: &HermesSessionContext,
) -> HermesSourceBackedResult<CoreRecord> {
    let native = match prepared {
        Some(prepared) => prepared.native,
        None => hermes_native_event(&row, ordinal)?,
    };
    let body = native.complete_text;
    let native_item_key = NativeItemKey::composite(
        HERMES_MESSAGE_NAMESPACE,
        vec![TypedKey::utf8(&row.session_id)?, TypedKey::I64(row.id)],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: HERMES_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(&row.session_id)?,
        TypedKey::I64(row.id),
    ])?;
    let native_tool = (row.tool_name.is_some()
        || row.tool_call_id.is_some()
        || row.tool_calls.is_some())
    .then(|| {
        serde_json::json!({
            "name": row.tool_name,
            "call_id": row.tool_call_id,
            "calls": row.tool_calls,
        })
    });
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        session.session_id,
        source.clone(),
        native.provider_event_index,
        native.event_type.as_str(),
        session.agent_type.clone(),
        true,
        HERMES_SOURCE_PARSER_REVISION,
        body,
    )?;
    if let Some(parent_session_id) = session.parent_session_id {
        let kind = if session.is_primary {
            SessionRelationshipKind::RelatedUnknown
        } else {
            SessionRelationshipKind::Delegated
        };
        record.set_session_relationship(kind, Some(parent_session_id), session.root_session_id)?;
    }
    record.provider_session_id = Some(row.session_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(native.occurred_at.timestamp_millis());
    record.role = native.role.map(|role| role.as_str().to_owned());
    record.branch = session.branch.clone();
    record.workspace = session.workspace.clone();
    record.cwd = session.cwd.clone();
    if let Some(native_tool) = native_tool {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_tool": native_tool,
        }));
    }
    record.validate_contract()?;
    Ok(record)
}

fn bound_projected_record(
    record: HermesSourceBackedRecord,
) -> HermesSourceBackedResult<(HermesSourceBackedRecord, usize)> {
    let owned_bytes = projected_owned_bytes(&record)?;
    if owned_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Ok((record, owned_bytes));
    }
    let record = rejected(format!(
        "Hermes projected row requires {owned_bytes} bytes and exceeds the {}-byte page limit",
        NATIVE_INGESTION_PAGE_MAX_BYTES
    ));
    let owned_bytes = projected_owned_bytes(&record)?;
    Ok((record, owned_bytes))
}

fn projected_owned_bytes(record: &HermesSourceBackedRecord) -> Result<usize, serde_json::Error> {
    let fixed = 1024_usize;
    match record {
        HermesSourceBackedRecord::Session(session) => Ok(fixed
            .saturating_add(session.provider_session_id.len())
            .saturating_add(
                session
                    .provider_parent_session_id
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )
            .saturating_add(session.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.source_path.len())
            .saturating_add(session.agent_type.len())
            .saturating_add(session.workspace.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.cwd.as_deref().map(str::len).unwrap_or(0))),
        HermesSourceBackedRecord::Event(event) => {
            Ok(fixed.saturating_add(serde_json::to_vec(event)?.len()))
        }
        HermesSourceBackedRecord::Rejected(rejection) => {
            Ok(fixed.saturating_add(rejection.reason.len()))
        }
    }
}

fn native_record_digest(native: &HermesNativeRow) -> HermesSourceBackedResult<[u8; 32]> {
    match &native.record {
        HermesNativeRecord::Session(row) => Ok(session_record_digest(row)),
        HermesNativeRecord::Message {
            values, prepared, ..
        } => {
            if !values.is_empty() {
                decode_sha256(hermes_layout_record_digest(values).as_str())
            } else if let Some(prepared) = prepared {
                decode_sha256(prepared.record_digest.as_str())
            } else {
                Err(HermesSourceBackedError::InvalidLogicalDigest)
            }
        }
        HermesNativeRecord::Rejected(reason) => {
            let mut digest = Sha256::new();
            digest.update(HERMES_REJECTION_DIGEST_DOMAIN);
            digest.update(reason.as_bytes());
            Ok(digest.finalize().into())
        }
    }
}

fn session_record_digest(row: &HermesSessionRow) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_SESSION_DIGEST_DOMAIN);
    hash_text(&mut digest, &row.id);
    hash_text(&mut digest, &row.source);
    hash_optional_text(&mut digest, row.parent_session_id.as_deref());
    hash_optional_text(&mut digest, row.model.as_deref());
    hash_optional_text(&mut digest, row.model_config.as_deref());
    digest.update(row.started_at.to_bits().to_be_bytes());
    hash_optional_f64(&mut digest, row.ended_at);
    hash_optional_text(&mut digest, row.end_reason.as_deref());
    digest.update(row.message_count.to_be_bytes());
    digest.update(row.tool_call_count.to_be_bytes());
    digest.update(row.input_tokens.to_be_bytes());
    digest.update(row.output_tokens.to_be_bytes());
    digest.update(row.cache_read_tokens.to_be_bytes());
    digest.update(row.cache_write_tokens.to_be_bytes());
    digest.update(row.reasoning_tokens.to_be_bytes());
    hash_optional_text(&mut digest, row.cwd.as_deref());
    hash_optional_text(&mut digest, row.git_branch.as_deref());
    hash_optional_text(&mut digest, row.git_repo_root.as_deref());
    hash_optional_text(&mut digest, row.billing_provider.as_deref());
    hash_optional_text(&mut digest, row.billing_base_url.as_deref());
    hash_optional_text(&mut digest, row.billing_mode.as_deref());
    hash_optional_f64(&mut digest, row.estimated_cost_usd);
    hash_optional_f64(&mut digest, row.actual_cost_usd);
    hash_optional_text(&mut digest, row.title.as_deref());
    digest.update(row.archived.to_be_bytes());
    digest.finalize().into()
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hash_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_f64(digest: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn decode_sha256(value: &str) -> HermesSourceBackedResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(HermesSourceBackedError::InvalidLogicalDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn decode_hex_nibble(value: u8) -> HermesSourceBackedResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(HermesSourceBackedError::InvalidLogicalDigest),
    }
}

#[cfg(test)]
mod tests;
