use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CaptureProvider, CertifiedSource, CoreActivity,
    CoreRecord, CoreRecordError, EventIdentityInput, LiteralFactKind, NativeItemKey,
    NativeSessionKey, ProjectionContractError, ProviderDeclaredFact, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SubrecordSelector, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use thiserror::Error;

use super::scanner::{
    absolute_trae_path, acquire_source, packed_native_index, TraeCoreRecord, TraeFrontier,
    TraeScanner, TraeSourceAuthority,
};
use crate::{
    provider_limits::TRAE_SOURCE_BACKED_PAGE_MAX_UNITS, sqlite_source::SqliteSourceEvidence,
    CaptureError,
};
use ctx_history_provider_runtime::SqliteLogicalSnapshot;

use super::super::TRAE_STATE_VSCDB_SOURCE_FORMAT;

mod replacement;

pub use replacement::TraeReplacementTree;

const TRAE_SOURCE_ANCHOR_NAMESPACE: &str = "trae.workspace-storage";
const TRAE_SOURCE_SCHEMA_VARIANT: &str = "trae-itemtable-json-v1";
const TRAE_SOURCE_BACKED_PARSER_REVISION: &str = "trae-itemtable-source-backed-v2-core-activity";
const TRAE_NATIVE_SESSION_NAMESPACE: &str = "trae.itemtable-session-v1";
const TRAE_SESSION_POSITION_KIND: &str = "trae.itemtable-session-position-v1";
const TRAE_NATIVE_ITEM_NAMESPACE: &str = "trae.itemtable-key-v1";
const TRAE_NATIVE_MESSAGE_NAMESPACE: &str = "trae.itemtable-message-v1";
const TRAE_MESSAGE_POSITION_KIND: &str = "trae.itemtable-message-position-v1";
const TRAE_LOGICAL_SESSION_KIND: &str = "trae-session";
const TRAE_LOGICAL_EVENT_KIND: &str = "trae-message";

#[derive(Debug, Error)]
pub(crate) enum TraeSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("Trae source-backed adapter requires an explicit state.vscdb leaf")]
    ExplicitLeafRequired,
    #[error("Trae source-backed scan counters overflowed or did not reconcile")]
    CountMismatch,
}

pub(crate) type TraeSourceBackedResultV0<T> = std::result::Result<T, TraeSourceBackedErrorV0>;

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedPageV0 {
    pub(crate) documents: Vec<CoreRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedScanV0 {
    pub(crate) source: CertifiedSource,
    pub(crate) terminal_fence: TraeSourceTerminalFence,
    pub(crate) row_decode_passes: u64,
    pub(crate) decoded_rows: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) peak_buffered_documents: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceTerminalFence {
    evidence: SqliteSourceEvidence,
}

pub(super) fn scan_trae_authority(
    canonical_path: &Path,
    authority: &TraeSourceAuthority,
    emit: &mut dyn FnMut(TraeSourceBackedPageV0) -> TraeSourceBackedResultV0<()>,
) -> TraeSourceBackedResultV0<TraeSourceBackedScanV0> {
    let source = source_key(authority)?;
    let mut scanner = TraeScanner::new(authority, TraeFrontier::default());
    let mut counts = ScannedSourceCounts::default();
    let mut emitted_pages = 0_u64;
    let mut peak_buffered_documents = 0_u64;
    while let Some(page) = scanner.next_page()? {
        let complete_records = u64::try_from(page.logical_units)
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let rejected_records = u64::try_from(page.rejections.len())
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let mut documents = Vec::with_capacity(page.core.len());
        for record in page.core {
            if let Some(document) = core_record(&source, authority, record)? {
                documents.push(document);
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let ignored_records = complete_records
            .checked_sub(
                retained_records
                    .checked_add(rejected_records)
                    .ok_or(TraeSourceBackedErrorV0::CountMismatch)?,
            )
            .ok_or(TraeSourceBackedErrorV0::CountMismatch)?;

        counts.complete_records = checked_add(counts.complete_records, complete_records)?;
        counts.retained_records = checked_add(counts.retained_records, retained_records)?;
        counts.rejected_records = checked_add(counts.rejected_records, rejected_records)?;
        counts.ignored_records = checked_add(counts.ignored_records, ignored_records)?;
        counts.indexed_documents = checked_add(counts.indexed_documents, retained_records)?;
        peak_buffered_documents = peak_buffered_documents.max(retained_records);
        if !documents.is_empty() {
            if documents.len() > TRAE_SOURCE_BACKED_PAGE_MAX_UNITS {
                return Err(TraeSourceBackedErrorV0::CountMismatch);
            }
            emitted_pages = checked_add(emitted_pages, 1)?;
            emit(TraeSourceBackedPageV0 { documents })?;
        }
    }

    let terminal_evidence = authority.database.seal(canonical_path)?;
    authority.workspace_folder.revalidate()?;
    counts.certified_bytes = scanner.certified_source_bytes();
    let decoded_rows = scanner.decoded_rows();
    let source = SqliteLogicalSnapshot::new(
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        &authority.schema_evidence,
        scanner.source_content_digest(),
        counts,
    )
    .certify(source)?;
    Ok(TraeSourceBackedScanV0 {
        source,
        terminal_fence: TraeSourceTerminalFence {
            evidence: terminal_evidence,
        },
        row_decode_passes: 1,
        decoded_rows,
        emitted_pages,
        peak_buffered_documents,
    })
}

fn core_record(
    source: &SourceKey,
    authority: &TraeSourceAuthority,
    record: TraeCoreRecord,
) -> TraeSourceBackedResultV0<Option<CoreRecord>> {
    let body = record.lexical_text;
    if body.is_empty() {
        return Ok(None);
    }
    let revision_scope = TypedKey::bytes(record.value_digest.to_vec())?;
    let session_key = if record.native_session_id_from_provider {
        NativeSessionKey::composite(
            TRAE_NATIVE_SESSION_NAMESPACE,
            vec![
                TypedKey::utf8(record.chat_key)?,
                TypedKey::utf8(&record.native_session_id)?,
            ],
        )?
    } else {
        NativeSessionKey::revision_scoped_position(
            TRAE_SESSION_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.key_index)),
                TypedKey::U64(u64::from(record.raw_session_index)),
            ])?,
            revision_scope.clone(),
        )?
    };
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: TRAE_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let item_key =
        NativeItemKey::native_id(TRAE_NATIVE_ITEM_NAMESPACE, TypedKey::utf8(record.chat_key)?)?;
    let subrecord = if record.native_message_id_from_provider {
        SubrecordSelector::composite(
            TRAE_NATIVE_MESSAGE_NAMESPACE,
            vec![
                TypedKey::utf8(&record.native_session_id)?,
                TypedKey::utf8(&record.native_message_id)?,
            ],
        )?
    } else {
        SubrecordSelector::revision_scoped_position(
            TRAE_MESSAGE_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.raw_session_index)),
                TypedKey::U64(u64::from(record.message_index)),
            ])?,
            revision_scope,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: TRAE_LOGICAL_EVENT_KIND,
        native_item_key: &item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(record.chat_key)?,
        TypedKey::U64(u64::from(record.raw_session_index)),
        TypedKey::U64(u64::from(record.message_index)),
        TypedKey::utf8(&record.provider_session_id)?,
    ])?;
    let event_sequence = packed_native_index(
        record.key_index,
        record.raw_session_index,
        record.message_index,
    )?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event_sequence,
        record.event_type.as_str(),
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    projected.agent_scope = Some(AgentScope::Primary);
    projected.provider_session_id = Some(record.provider_session_id);
    projected.native_event_id = Some(native_event_id);
    projected.occurred_at_unix_ms = Some(record.occurred_at.timestamp_millis());
    projected.role = record.role.map(|role| role.as_str().to_owned());
    let facts = authority.workspace_folder.literal().map_or_else(
        || {
            vec![ProviderDeclaredFact {
                kind: LiteralFactKind::Workspace,
                value: authority.workspace_id.clone(),
            }]
        },
        |folder| {
            vec![
                ProviderDeclaredFact {
                    kind: LiteralFactKind::Workspace,
                    value: folder.to_owned(),
                },
                ProviderDeclaredFact {
                    kind: LiteralFactKind::SessionCwd,
                    value: folder.to_owned(),
                },
            ]
        },
    );
    projected.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts,
    });
    projected.validate_contract()?;
    Ok(Some(projected))
}

fn source_key(authority: &TraeSourceAuthority) -> TraeSourceBackedResultV0<SourceKey> {
    source_key_for_workspace(&authority.workspace_id)
}

fn source_key_for_workspace(workspace_id: &str) -> TraeSourceBackedResultV0<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(TRAE_SOURCE_ANCHOR_NAMESPACE, TypedKey::utf8(workspace_id)?)?;
    Ok(SourceKey::derive(
        CaptureProvider::Trae.as_str(),
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        TRAE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn explicit_trae_leaf(path: &Path) -> TraeSourceBackedResultV0<PathBuf> {
    ctx_history_provider_runtime::source_io::ensure_provider_path_parents_are_not_symlinks(path)?;
    let metadata = fs::symlink_metadata(path).map_err(CaptureError::from)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(TraeSourceBackedErrorV0::ExplicitLeafRequired);
    }
    if !matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("state.vscdb" | "database.db")
    ) {
        return Err(TraeSourceBackedErrorV0::ExplicitLeafRequired);
    }
    Ok(absolute_trae_path(path)?)
}

fn checked_add(left: u64, right: u64) -> TraeSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(TraeSourceBackedErrorV0::CountMismatch)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::{
        derive_event_id, EventIdentityInput, NativeItemKey, SubrecordSelector, TypedKey,
    };
    use rusqlite::{params, Connection};
    use sha2::{Digest, Sha256};

    use super::{
        scan_trae_authority, TRAE_LOGICAL_EVENT_KIND, TRAE_MESSAGE_POSITION_KIND,
        TRAE_NATIVE_ITEM_NAMESPACE, TRAE_NATIVE_MESSAGE_NAMESPACE,
    };
    use crate::trae::nativepath::scanner::acquire_source;

    #[test]
    fn direct_core_projection_is_self_contained() {
        let production = include_str!("source_backed.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("production source");
        assert!(production.contains("CoreRecord::new_selected"));
        assert!(production.contains("native_event_id = Some"));
        assert!(production.contains("TRAE_SOURCE_BACKED_PARSER_REVISION"));
        assert!(production.contains("let body = record.lexical_text"));
        assert!(production.contains("validate_contract"));
        assert!(!production.contains("body.truncate"));
        assert!(!production.contains("body.chars().take"));
        for removed_api in [
            concat!("Lexical", "Document"),
            concat!("SourceRecord", "Locator"),
            concat!("hyd", "rate_"),
            concat!("resol", "ver"),
        ] {
            assert!(!production.contains(removed_api), "found {removed_api}");
        }
    }

    #[test]
    fn blank_first_aliases_keep_positional_event_identity_and_workspace_fallback() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = crate::test_support_paths::tempdir().unwrap();
        let workspace_root = temp.path().join("blank-first-workspace");
        fs::create_dir(&workspace_root).unwrap();
        let source_path = workspace_root.join("state.vscdb");
        fs::write(
            workspace_root.join("workspace.json"),
            r#"{"folder":"  ","workspace":"file:///must/not/win"}"#,
        )
        .unwrap();
        let payload = r#"{"list":[{"id":"native-session","messages":[{"id":"  ","messageId":"later-native-message","role":"  ","type":"assistant","content":"historical identity"},{"id":"output","role":"  ","type":"toolResult","content":"must remain output"}]}]}"#;
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
                params![crate::TRAE_CHAT_KEYS[0], payload],
            )
            .unwrap();
        drop(connection);

        let authority = acquire_source(
            data_root.path(),
            &source_path,
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        )
        .unwrap();
        let mut documents = Vec::new();
        let scan = scan_trae_authority(&source_path, &authority, &mut |page| {
            documents.extend(page.documents);
            Ok(())
        })
        .unwrap();

        assert_eq!(scan.source.counts().complete_records, 2);
        assert_eq!(scan.source.counts().retained_records, 1);
        assert_eq!(scan.source.counts().ignored_records, 1);
        assert_eq!(documents.len(), 1);
        let record = &documents[0];
        assert_eq!(
            record.agent_scope,
            Some(ctx_history_core::AgentScope::Primary)
        );
        let item_key = NativeItemKey::native_id(
            TRAE_NATIVE_ITEM_NAMESPACE,
            TypedKey::utf8(crate::TRAE_CHAT_KEYS[0]).unwrap(),
        )
        .unwrap();
        let positional_subrecord = SubrecordSelector::revision_scoped_position(
            TRAE_MESSAGE_POSITION_KIND,
            TypedKey::composite(vec![TypedKey::U64(0), TypedKey::U64(0)]).unwrap(),
            TypedKey::bytes(Sha256::digest(payload.as_bytes()).to_vec()).unwrap(),
        )
        .unwrap();
        let expected_event_id = derive_event_id(EventIdentityInput {
            source: &record.source,
            session_id: record.session_id,
            logical_item_kind: TRAE_LOGICAL_EVENT_KIND,
            native_item_key: &item_key,
            subrecord_selector: Some(&positional_subrecord),
        })
        .unwrap();
        let later_alias_subrecord = SubrecordSelector::composite(
            TRAE_NATIVE_MESSAGE_NAMESPACE,
            vec![
                TypedKey::utf8("native-session").unwrap(),
                TypedKey::utf8("later-native-message").unwrap(),
            ],
        )
        .unwrap();
        let later_alias_event_id = derive_event_id(EventIdentityInput {
            source: &record.source,
            session_id: record.session_id,
            logical_item_kind: TRAE_LOGICAL_EVENT_KIND,
            native_item_key: &item_key,
            subrecord_selector: Some(&later_alias_subrecord),
        })
        .unwrap();

        assert_eq!(record.event_id, expected_event_id);
        assert_ne!(record.event_id, later_alias_event_id);
        assert_eq!(record.role.as_deref(), Some("unknown"));
        assert_eq!(
            record.content.activity.as_ref().unwrap().facts,
            vec![ctx_history_core::ProviderDeclaredFact {
                kind: ctx_history_core::LiteralFactKind::Workspace,
                value: "blank-first-workspace".to_owned(),
            }]
        );
        assert_eq!(
            record.parser_revision,
            "trae-itemtable-source-backed-v2-core-activity"
        );
    }

    #[test]
    fn exact_workspace_folder_is_preserved_verbatim_as_workspace_and_cwd_facts() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = crate::test_support_paths::tempdir().unwrap();
        let workspace_root = temp.path().join("exact-workspace");
        fs::create_dir(&workspace_root).unwrap();
        let source_path = workspace_root.join("state.vscdb");
        let literal = "file:///Users/Case%20Sensitive/./repo";
        fs::write(
            workspace_root.join("workspace.json"),
            serde_json::to_vec(&serde_json::json!({"folder": literal})).unwrap(),
        )
        .unwrap();
        let payload = r#"{"list":[{"id":"native-session","messages":[{"id":"native-message","role":"assistant","content":"literal workspace"}]}]}"#;
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
                params![crate::TRAE_CHAT_KEYS[0], payload],
            )
            .unwrap();
        drop(connection);

        let authority = acquire_source(
            data_root.path(),
            &source_path,
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        )
        .unwrap();
        let mut documents = Vec::new();
        scan_trae_authority(&source_path, &authority, &mut |page| {
            documents.extend(page.documents);
            Ok(())
        })
        .unwrap();

        assert_eq!(documents.len(), 1);
        let record = &documents[0];
        assert_eq!(
            record.content.activity.as_ref().unwrap().facts,
            vec![
                ctx_history_core::ProviderDeclaredFact {
                    kind: ctx_history_core::LiteralFactKind::Workspace,
                    value: literal.to_owned(),
                },
                ctx_history_core::ProviderDeclaredFact {
                    kind: ctx_history_core::LiteralFactKind::SessionCwd,
                    value: literal.to_owned(),
                },
            ]
        );
        assert_eq!(
            record.source,
            super::source_key_for_workspace("exact-workspace").unwrap()
        );
    }

    #[test]
    fn oversized_workspace_metadata_abstains_without_deriving_location_semantics() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = crate::test_support_paths::tempdir().unwrap();
        let workspace_root = temp.path().join("bounded-workspace");
        fs::create_dir(&workspace_root).unwrap();
        let source_path = workspace_root.join("state.vscdb");
        fs::write(
            workspace_root.join("workspace.json"),
            vec![b'x'; crate::MAX_PROVIDER_JSONL_LINE_BYTES + 1],
        )
        .unwrap();
        let payload = r#"{"list":[{"id":"native-session","messages":[{"id":"native-message","role":"assistant","content":"bounded workspace"}]}]}"#;
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
                params![crate::TRAE_CHAT_KEYS[0], payload],
            )
            .unwrap();
        drop(connection);

        let authority = acquire_source(
            data_root.path(),
            &source_path,
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        )
        .unwrap();
        let mut documents = Vec::new();
        scan_trae_authority(&source_path, &authority, &mut |page| {
            documents.extend(page.documents);
            Ok(())
        })
        .unwrap();

        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].content.activity.as_ref().unwrap().facts,
            vec![ctx_history_core::ProviderDeclaredFact {
                kind: ctx_history_core::LiteralFactKind::Workspace,
                value: "bounded-workspace".to_owned(),
            }]
        );
    }
}
