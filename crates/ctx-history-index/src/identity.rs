use std::collections::HashMap;

use ctx_history_core::{CoreRecord, StableEntityId};
use tantivy::{
    schema::IndexRecordOption, DocAddress, DocSet, Searcher, TantivyDocument, Term, TERMINATED,
};
use uuid::Uuid;

use crate::{
    hex, merge_session_identity_facts, preparation::PreparedSessionIdentityFacts, Fields,
    IndexError, Result,
};
use ctx_history_index_format::unique_required_bytes;

/// Reads one exact session claim from the immutable, already-verified base.
///
/// A session may own many events. Absence of one provider-native claim is
/// unknown, so every live posting participates in one deterministic,
/// commutative merge. This avoids selecting an arbitrary posting when an older
/// event lacks a claim that a later event supplies.
pub(crate) fn prior_session_identity_facts(
    searcher: &Searcher,
    fields: Fields,
    session_id: StableEntityId,
) -> Result<Option<PreparedSessionIdentityFacts>> {
    let term = Term::from_field_text(fields.session_id, &session_id.as_uuid().to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(None);
    }

    let mut merged = None;
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(fields.session_id)?;
        let Some(mut postings) = inverted.read_postings(&term, IndexRecordOption::Basic)? else {
            continue;
        };
        let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
        let mut doc_id = postings.doc();
        while doc_id != TERMINATED {
            if !segment.is_deleted(doc_id) {
                let document: TantivyDocument =
                    searcher.doc(DocAddress::new(segment_ord, doc_id))?;
                let encoded = unique_required_bytes(&document, fields.core_record, "core_record")?;
                let record = CoreRecord::decode_stored(encoded)?;
                if record.session_id.as_uuid() != session_id.as_uuid() {
                    return Err(IndexError::InvalidStoredDocumentField("core_record"));
                }
                let facts = PreparedSessionIdentityFacts::for_core_record(&record);
                merged = Some(match merged {
                    Some(existing) => merge_session_identity_facts(existing, facts)?,
                    None => facts,
                });
            }
            doc_id = postings.advance();
        }
    }
    Ok(merged)
}

pub(crate) fn register_compact_identity(
    identities: &mut HashMap<Uuid, [u8; 32]>,
    identity: StableEntityId,
    kind: &'static str,
    duplicate_is_error: bool,
) -> Result<()> {
    let uuid = identity.as_uuid();
    let digest = identity.digest();
    match identities.entry(uuid) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(digest);
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(entry) if *entry.get() == digest => {
            if duplicate_is_error {
                Err(IndexError::DuplicateEventIdentity(uuid.to_string()))
            } else {
                Ok(())
            }
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            Err(IndexError::CompactIdentityCollision {
                kind,
                uuid,
                existing_digest: hex(entry.get()),
                new_digest: hex(&digest),
            })
        }
    }
}
