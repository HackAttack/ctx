use std::collections::HashMap;

use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};
use tantivy::{schema::IndexRecordOption, Searcher, Term};
use uuid::Uuid;

use crate::{hex, Fields, IndexError, Result};

pub(crate) fn prior_core_record(
    searcher: &Searcher,
    fields: Fields,
    identity: StableEntityId,
    current_source: &SourceKey,
) -> Result<Option<CoreRecord>> {
    use tantivy::{collector::TopDocs, query::TermQuery};

    let term = Term::from_field_text(fields.event_id, &identity.as_uuid().to_string());
    if searcher.doc_freq(&term)? == 0 {
        return Ok(None);
    }
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let hits = searcher.search(&query, &TopDocs::with_limit(2).order_by_score())?;
    if hits.len() > 1 {
        return Err(IndexError::DuplicateEventIdentity(
            identity.as_uuid().to_string(),
        ));
    }
    let Some((_, address)) = hits.into_iter().next() else {
        return Ok(None);
    };
    let record = crate::query::stored_verification_record(searcher, address, fields)?;
    if record.core_record.event_id != identity {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    if !record
        .core_record
        .source
        .exact_descriptor_eq(current_source)
    {
        return Ok(None);
    }
    Ok(Some(record.core_record))
}

pub(crate) fn source_token(source: &SourceKey) -> String {
    hex(&source.identity().digest())
}

pub(crate) fn source_sort_key(source: &SourceKey) -> [u8; 32] {
    source.identity().digest()
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
