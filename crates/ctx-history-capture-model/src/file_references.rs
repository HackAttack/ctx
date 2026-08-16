use ctx_history_core::LiteralFactKind;
use serde_json::Value;

// Legacy packed provider identity reserves the low 16 bits for one reference
// within an event. Full-width event identities retain this ordinal separately.
pub const MAX_PROVIDER_FILE_REFERENCES_PER_EVENT: usize = 1 << 16;
pub const MAX_PACKED_PROVIDER_EVENT_INDEX: u64 = u64::MAX >> 16;
const MAX_PROVIDER_FIELD_NAME_BYTES: usize = 256;
const MAX_PROVIDER_LITERAL_VALUE_BYTES: usize = 16 * 1024;
pub const PROVIDER_FILE_REFERENCE_LIMIT_REJECTION: &str =
    "provider event exceeds the 65,536 literal file-reference limit";

/// One exact literal selected from a closed provider field allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReferenceDraft {
    pub kind: LiteralFactKind,
    pub value: String,
    pub native_field: String,
}

enum ProviderFileReferenceTraversalError<E> {
    Sink(E),
    EventReferenceLimitExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderFileReferenceVisitOutcome {
    emitted: usize,
    limit_exceeded: bool,
}

impl ProviderFileReferenceVisitOutcome {
    pub fn emitted(self) -> usize {
        self.emitted
    }

    pub fn limit_exceeded(self) -> bool {
        self.limit_exceeded
    }
}

pub fn visit_literal_file_reference_drafts<E>(
    raw_value: &Value,
    mut visit: impl FnMut(FileReferenceDraft) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    visit_structured_literals(raw_value, &mut visit)
}

pub fn visit_provider_file_reference_drafts_with_limit<E>(
    raw_value: &Value,
    reference_limit: usize,
    mut visit: impl FnMut((u64, FileReferenceDraft)) -> std::result::Result<(), E>,
) -> std::result::Result<ProviderFileReferenceVisitOutcome, E> {
    let mut emitted = 0_usize;
    let mut emit = |draft: FileReferenceDraft| {
        if emitted == reference_limit {
            return Err(ProviderFileReferenceTraversalError::EventReferenceLimitExceeded);
        }
        visit((emitted as u64, draft)).map_err(ProviderFileReferenceTraversalError::Sink)?;
        emitted += 1;
        Ok(())
    };
    let limit_exceeded = match visit_structured_literals(raw_value, &mut emit) {
        Ok(()) => false,
        Err(ProviderFileReferenceTraversalError::Sink(error)) => return Err(error),
        Err(ProviderFileReferenceTraversalError::EventReferenceLimitExceeded) => true,
    };
    Ok(ProviderFileReferenceVisitOutcome {
        emitted,
        limit_exceeded,
    })
}

fn visit_structured_literals<E>(
    value: &Value,
    visit: &mut impl FnMut(FileReferenceDraft) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_structured_literals(item, visit)?;
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                if let Some(kind) = literal_fact_kind(field) {
                    if let Some(value) = value.as_str().filter(|value| {
                        !value.is_empty() && value.len() <= MAX_PROVIDER_LITERAL_VALUE_BYTES
                    }) {
                        visit(FileReferenceDraft {
                            kind,
                            value: value.to_owned(),
                            native_field: field.clone(),
                        })?;
                    }
                }
                visit_structured_literals(value, visit)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn literal_fact_kind(field: &str) -> Option<LiteralFactKind> {
    match bounded_normalized_key(field)?.as_str() {
        "path" | "file" | "filepath" | "filename" | "targetfile" | "targetpath"
        | "relativepath" | "absolutepath" | "destinationfile" | "destinationpath" | "oldpath"
        | "frompath" | "sourcepath" | "originalpath" | "previouspath" => {
            Some(LiteralFactKind::File)
        }
        "url" | "uri" => Some(LiteralFactKind::Url),
        _ => None,
    }
}

fn bounded_normalized_key(field: &str) -> Option<String> {
    if field.len() > MAX_PROVIDER_FIELD_NAME_BYTES {
        return None;
    }
    Some(
        field
            .bytes()
            .filter(u8::is_ascii_alphanumeric)
            .map(|byte| char::from(byte.to_ascii_lowercase()))
            .collect(),
    )
}

#[cfg(test)]
#[path = "file_references_tests.rs"]
mod tests;
