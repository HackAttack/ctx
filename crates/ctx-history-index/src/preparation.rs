use std::{
    fmt,
    io::{self, Write},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    CoreRecord, ProviderNativeSessionRelationship, SourceKey, StableEntityId,
    CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION, MAX_ENCODED_CORE_RECORD_BYTES,
};

use crate::{
    core_record_accumulator_leaf, core_record_leaf, Fields, IndexDocument, IndexError, Result,
};

#[cfg(test)]
thread_local! {
    static FINAL_ENCODINGS: Cell<usize> = const { Cell::new(0) };
}

/// Immutable authority for canonical Core-record preparation.
///
/// Clones may run concurrently and cannot mutate source lifecycle or
/// publication state.
#[derive(Clone)]
pub struct CoreRecordPreparer {
    fields: Fields,
    context_generation_id: Option<String>,
}

impl CoreRecordPreparer {
    pub(crate) fn new(fields: Fields, context_generation_id: Option<String>) -> Self {
        Self {
            fields,
            context_generation_id,
        }
    }

    /// Performs exactly one final canonical encoding and derives every lexical
    /// projection and aggregate leaf from those same bytes. Preparation never
    /// imports semantics from a prior generation or persists publication state.
    pub fn prepare(&self, record: CoreRecord) -> Result<PreparedCoreRecord> {
        match self
            .prepare_draft(record)?
            .materialize(MAX_ENCODED_CORE_RECORD_BYTES)?
        {
            PreparedCoreRecordMaterialization::Prepared(prepared) => Ok(prepared),
            PreparedCoreRecordMaterialization::CapacityExceeded(_) => {
                Err(IndexError::DocumentFieldTooLarge {
                    field: "core_record",
                    actual: MAX_ENCODED_CORE_RECORD_BYTES.saturating_add(1),
                    maximum: MAX_ENCODED_CORE_RECORD_BYTES,
                })
            }
        }
    }

    /// Validates one record without allocating its canonical stored encoding or
    /// lexical document. Callers that govern cross-thread memory can therefore
    /// acquire a permit before materialization begins.
    pub fn prepare_draft(&self, record: CoreRecord) -> Result<PreparedCoreRecordDraft> {
        if record.normalization_revision != CORE_NORMALIZATION_REVISION
            || record.content.policy_revision != CORE_CONTENT_POLICY_REVISION
        {
            return Err(IndexError::CoreRecordPolicyRevisionMismatch {
                normalization: record.normalization_revision,
                expected_normalization: CORE_NORMALIZATION_REVISION,
                content: record.content.policy_revision,
                expected_content: CORE_CONTENT_POLICY_REVISION,
            });
        }
        let core_content_bytes = record.validate_contract_and_content_bytes()?;
        let source = record.source.clone();
        let source_token = crate::source_token(&source);
        Ok(PreparedCoreRecordDraft {
            fields: self.fields,
            base_generation_id: self.context_generation_id.clone(),
            record,
            source,
            source_token,
            core_content_bytes,
        })
    }
}

/// Opaque validated Core preparation state that has not allocated the final
/// stored encoding or index document.
pub struct PreparedCoreRecordDraft {
    fields: Fields,
    base_generation_id: Option<String>,
    record: CoreRecord,
    source: SourceKey,
    source_token: String,
    core_content_bytes: usize,
}

/// Result of attempting final materialization under a caller-owned exact-byte
/// permit. Capacity exhaustion returns the untouched draft so a bounded
/// scheduler can flush, acquire a larger permit, and retry.
// Boxing the ordinary prepared result would add one allocation to every
// indexed record. Keep the hot success path inline and box only the uncommon
// capacity retry.
#[allow(clippy::large_enum_variant)]
pub enum PreparedCoreRecordMaterialization {
    Prepared(PreparedCoreRecord),
    CapacityExceeded(Box<PreparedCoreRecordDraft>),
}

impl PreparedCoreRecordDraft {
    pub fn materialize(
        self,
        maximum_encoded_bytes: usize,
    ) -> Result<PreparedCoreRecordMaterialization> {
        let maximum_encoded_bytes = maximum_encoded_bytes.min(MAX_ENCODED_CORE_RECORD_BYTES);
        let mut encoded = BoundedJsonBuffer::new(maximum_encoded_bytes);
        if let Err(error) = serde_json::to_writer(&mut encoded, &self.record) {
            if encoded.capacity_exceeded() {
                return Ok(PreparedCoreRecordMaterialization::CapacityExceeded(
                    Box::new(self),
                ));
            }
            return Err(error.into());
        }
        let encoded_core_record = encoded.into_bytes();
        let encoded_core_bytes = encoded_core_record.len();
        if encoded_core_bytes == 0 {
            return Err(IndexError::EmptyDocumentField {
                field: "core_record",
            });
        }
        #[cfg(test)]
        FINAL_ENCODINGS.with(|count| count.set(count.get() + 1));
        let event_id = self.record.event_id;
        let identity_facts = PreparedCoreIdentityFacts {
            event_id,
            session: PreparedSessionIdentityFacts {
                session_id: self.record.session_id,
                source_owner: self.record.source.identity().digest(),
                relationship: PreparedSessionRelationship {
                    parent_session_id: self.record.parent_session_id,
                    root_session_id: self.record.root_session_id,
                    kind: self.record.session_relationship,
                },
            },
        };
        let record_leaf = core_record_leaf(event_id, &encoded_core_record)?;
        let record_accumulator_leaf = core_record_accumulator_leaf(event_id, &record_leaf)?;
        let document = IndexDocument::from_core(
            self.fields,
            self.record,
            encoded_core_record,
            self.core_content_bytes,
        )?;

        Ok(PreparedCoreRecordMaterialization::Prepared(
            PreparedCoreRecord {
                base_generation_id: self.base_generation_id,
                source: self.source,
                source_token: self.source_token,
                encoded_core_bytes,
                record_accumulator_leaf,
                identity_facts,
                document,
            },
        ))
    }
}

struct BoundedJsonBuffer {
    bytes: Vec<u8>,
    maximum: usize,
    capacity_exceeded: bool,
}

impl BoundedJsonBuffer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            capacity_exceeded: false,
        }
    }

    fn capacity_exceeded(&self) -> bool {
        self.capacity_exceeded
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(bytes.len()) else {
            self.capacity_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded Core record byte count overflowed",
            ));
        };
        if next_len > self.maximum {
            self.capacity_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded Core record exceeded its materialization permit",
            ));
        }
        if next_len > self.bytes.capacity() {
            let next_capacity = self
                .bytes
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .min(self.maximum)
                .max(next_len);
            self.bytes
                .try_reserve_exact(next_capacity.saturating_sub(self.bytes.len()))
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Opaque immutable result of canonical Core preparation.
pub struct PreparedCoreRecord {
    base_generation_id: Option<String>,
    source: SourceKey,
    source_token: String,
    encoded_core_bytes: usize,
    record_accumulator_leaf: [u8; 32],
    identity_facts: PreparedCoreIdentityFacts,
    document: IndexDocument,
}

/// Identity and child-owned relationship authority captured from the exact
/// validated Core record used to create a prepared document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedCoreIdentityFacts {
    pub(crate) event_id: StableEntityId,
    pub(crate) session: PreparedSessionIdentityFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedSessionIdentityFacts {
    pub(crate) session_id: StableEntityId,
    pub(crate) source_owner: [u8; 32],
    pub(crate) relationship: PreparedSessionRelationship,
}

impl PreparedSessionIdentityFacts {
    pub(crate) fn for_core_record(record: &CoreRecord) -> Self {
        Self {
            session_id: record.session_id,
            source_owner: record.source.identity().digest(),
            relationship: PreparedSessionRelationship {
                parent_session_id: record.parent_session_id,
                root_session_id: record.root_session_id,
                kind: record.session_relationship,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedSessionRelationship {
    pub(crate) parent_session_id: Option<StableEntityId>,
    pub(crate) root_session_id: Option<StableEntityId>,
    pub(crate) kind: Option<ProviderNativeSessionRelationship>,
}

impl PreparedCoreRecord {
    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    /// Exact byte length of the final post-certificate canonical encoding.
    pub fn encoded_core_bytes(&self) -> usize {
        self.encoded_core_bytes
    }

    pub(crate) fn base_generation_id(&self) -> Option<&str> {
        self.base_generation_id.as_deref()
    }

    pub(crate) fn source_token(&self) -> &str {
        &self.source_token
    }

    pub(crate) fn into_parts(self) -> PreparedCoreRecordParts {
        PreparedCoreRecordParts {
            record_accumulator_leaf: self.record_accumulator_leaf,
            identity_facts: self.identity_facts,
            document: self.document,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_identity_facts_mut(&mut self) -> &mut PreparedCoreIdentityFacts {
        &mut self.identity_facts
    }
}

impl fmt::Debug for PreparedCoreRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCoreRecord")
            .field("source", &self.source)
            .field("encoded_core_bytes", &self.encoded_core_bytes)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PreparedCoreRecordParts {
    pub(crate) record_accumulator_leaf: [u8; 32],
    pub(crate) identity_facts: PreparedCoreIdentityFacts,
    pub(crate) document: IndexDocument,
}

#[cfg(test)]
pub(crate) fn reset_final_encoding_count() {
    FINAL_ENCODINGS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn final_encoding_count() -> usize {
    FINAL_ENCODINGS.with(Cell::get)
}
