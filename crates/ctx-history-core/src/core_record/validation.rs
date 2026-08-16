use crate::{SourceKey, StableEntityId, StableEntityKind};

use super::{CoreRecordError, CoreRecordResult};

pub(super) fn validate_owned_identity(
    identity: StableEntityId,
    expected_kind: StableEntityKind,
    source: &SourceKey,
) -> CoreRecordResult<()> {
    identity
        .validate_contract()
        .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
    if identity.entity_kind() != expected_kind
        || identity.source_digest() != source.identity().digest()
        || identity.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return Err(CoreRecordError::InvalidIdentityRelationship);
    }
    Ok(())
}

pub(super) fn validate_related_session_identity(identity: StableEntityId) -> CoreRecordResult<()> {
    identity
        .validate_contract()
        .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
    if identity.entity_kind() != StableEntityKind::Session {
        return Err(CoreRecordError::InvalidIdentityRelationship);
    }
    Ok(())
}

pub(super) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> CoreRecordResult<()> {
    if value.is_empty() {
        return Err(CoreRecordError::EmptyField { field });
    }
    validate_size(field, value.len(), maximum)
}

pub(super) fn validate_optional_text(
    field: &'static str,
    value: Option<&str>,
    maximum: usize,
) -> CoreRecordResult<()> {
    if let Some(value) = value {
        validate_text(field, value, maximum)?;
    }
    Ok(())
}

pub(super) fn validate_size(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> CoreRecordResult<()> {
    if actual > maximum {
        return Err(CoreRecordError::FieldTooLarge {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

pub(super) fn validate_count(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> CoreRecordResult<()> {
    if actual > maximum {
        return Err(CoreRecordError::TooManyItems {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}
