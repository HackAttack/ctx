use ctx_history_core::{StableEntityId, StableEntityKind};
use tantivy::{schema::Value, DocAddress, TantivyDocument};

use crate::{
    core_record_leaf, decode_core_record_bytes,
    index_document::{
        core_content_bytes, EventRangeOrderKey, SemanticEventOrderKey, SessionAuthorityKey,
        SessionEventOrderKey, SourceEventOrderKey, EVENT_RANGE_ORDER_KEY_LEN,
        SEMANTIC_EVENT_ORDER_KEY_LEN, SESSION_AUTHORITY_KEY_LEN, SESSION_EVENT_ORDER_KEY_LEN,
        SOURCE_EVENT_ORDER_KEY_LEN,
    },
    source_token, unique_required_bytes, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION,
};

const VERIFY_CORE_RECORD: u32 = 40;

pub struct VerificationRecord {
    pub core_record: ctx_history_core::CoreRecord,
    pub source_owner: String,
    pub core_record_leaf: [u8; 32],
    pub source_event_order: [u8; SOURCE_EVENT_ORDER_KEY_LEN],
    pub session_event_order: [u8; SESSION_EVENT_ORDER_KEY_LEN],
    pub session_authority: Option<[u8; SESSION_AUTHORITY_KEY_LEN]>,
    pub semantic_event_order: [u8; SEMANTIC_EVENT_ORDER_KEY_LEN],
    pub event_range_order: [u8; EVENT_RANGE_ORDER_KEY_LEN],
    pub identities: CompactVerificationIdentities,
    pub stored_core_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactIdentity {
    pub digest: [u8; 32],
}

impl From<StableEntityId> for CompactIdentity {
    fn from(identity: StableEntityId) -> Self {
        let compact = Self {
            digest: identity.digest(),
        };
        debug_assert_eq!(compact.as_uuid(), identity.as_uuid());
        compact
    }
}

impl CompactIdentity {
    pub fn as_uuid(self) -> uuid::Uuid {
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&self.digest[..16]);
        bytes[6] = 0x80 | (bytes[6] & 0x0f);
        bytes[8] = 0x80 | (bytes[8] & 0x3f);
        uuid::Uuid::from_bytes(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactVerificationIdentities {
    pub event: CompactIdentity,
    pub session: CompactIdentity,
    pub parent_session: Option<CompactIdentity>,
    pub root_session: Option<CompactIdentity>,
    pub session_source_owner: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub enum IdentityFieldRole {
    Session,
    ParentSession,
    RootSession,
}

pub fn validate_verification_projection(fields: Fields) -> Result<()> {
    if fields.core_record.field_id() != VERIFY_CORE_RECORD {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub fn stored_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<VerificationRecord> {
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded = unique_required_bytes(&document, fields.core_record, "core_record")?;
    let stored_core_bytes = encoded.len();
    let core = decode_core_record_bytes(searcher, address, encoded)?;
    if core.event_id.entity_kind() != StableEntityKind::Event
        || core.session_id.entity_kind() != StableEntityKind::Session
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    let core_record_leaf = core_record_leaf(core.event_id, encoded)?;
    let source_owner = source_token(&core.source);
    let source_event_order =
        SourceEventOrderKey::for_core_record(&core, stored_core_bytes)?.into_bytes();
    let session_event_order = SessionEventOrderKey::for_core_record(&core)?.into_bytes();
    let mut authority_values = document.get_all(fields.session_authority);
    let session_authority = match authority_values.next() {
        None => None,
        Some(value) if authority_values.next().is_none() => Some(
            SessionAuthorityKey::decode(
                value
                    .as_bytes()
                    .ok_or(IndexError::InvalidStoredDocumentField("session_authority"))?,
            )?
            .into_bytes(),
        ),
        Some(_) => return Err(IndexError::InvalidStoredDocumentField("session_authority")),
    };
    let semantic_event_order = SemanticEventOrderKey::for_event(core.event_id)?.into_bytes();
    let event_range_order = EventRangeOrderKey::for_core_record(
        &core,
        stored_core_bytes,
        core_content_bytes(&core.content)?,
    )?
    .into_bytes();
    let identities = CompactVerificationIdentities {
        event: core.event_id.into(),
        session: core.session_id.into(),
        parent_session: core.parent_session_id.map(CompactIdentity::from),
        root_session: core.root_session_id.map(CompactIdentity::from),
        session_source_owner: core.source.identity().digest(),
    };
    Ok(VerificationRecord {
        core_record: core,
        source_owner,
        core_record_leaf,
        source_event_order,
        session_event_order,
        session_authority,
        semantic_event_order,
        event_range_order,
        identities,
        stored_core_bytes,
    })
}
