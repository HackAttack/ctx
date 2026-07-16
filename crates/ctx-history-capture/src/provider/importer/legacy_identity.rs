use ctx_history_core::Session;
use ctx_history_store::{Store, StoreError};
use uuid::Uuid;

use super::provider_source_identity;
use crate::{CaptureError, Result};

pub(super) fn legacy_session_matches_source(
    store: &Store,
    session: &Session,
    source_id: Uuid,
    source_identity: &str,
) -> Result<bool> {
    let Some(existing_source_id) = session.capture_source_id else {
        return Ok(false);
    };
    if existing_source_id == source_id {
        return Ok(true);
    }
    let existing_source = match store.get_capture_source(existing_source_id) {
        Ok(source) => source,
        Err(StoreError::NotFound(_)) => return Ok(false),
        Err(err) => return Err(CaptureError::Store(err)),
    };
    let incoming_source = match store.get_capture_source(source_id) {
        Ok(source) => source,
        Err(StoreError::NotFound(_)) => return Ok(false),
        Err(err) => return Err(CaptureError::Store(err)),
    };
    if existing_source.descriptor.provider != incoming_source.descriptor.provider {
        return Ok(false);
    }

    let existing_source_format = existing_source
        .descriptor
        .source_format
        .as_deref()
        .or_else(|| existing_source.sync.metadata["source_format"].as_str());
    let incoming_source_format = incoming_source
        .descriptor
        .source_format
        .as_deref()
        .or_else(|| incoming_source.sync.metadata["source_format"].as_str());
    if matches!(
        (existing_source_format, incoming_source_format),
        (Some(existing), Some(incoming)) if existing != incoming
    ) {
        return Ok(false);
    }
    if existing_source.descriptor.source_identity.as_deref() == Some(source_identity) {
        return Ok(true);
    }
    if let Some(existing_source_format) = existing_source_format {
        let source_metadata = existing_source
            .sync
            .metadata
            .get("source_metadata")
            .unwrap_or(&existing_source.sync.metadata);
        let source_idempotency_key =
            existing_source.sync.metadata["source_idempotency_key"].as_str();
        if provider_source_identity(
            existing_source.descriptor.provider,
            existing_source_format,
            existing_source.descriptor.source_root.as_deref(),
            existing_source.descriptor.raw_source_path.as_deref(),
            source_idempotency_key,
            source_metadata,
        )
        .as_deref()
            == Some(source_identity)
        {
            return Ok(true);
        }
    }

    Ok(matches!(
        (
            existing_source.descriptor.raw_source_path.as_deref(),
            incoming_source.descriptor.raw_source_path.as_deref(),
        ),
        (Some(existing), Some(incoming)) if existing == incoming
    ))
}
