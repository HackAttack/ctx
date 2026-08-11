use ctx_history_capture_runtime::{
    BaseEventLookup, CoreMaterialization, CorePreparationError, CorePreparationFailureKind,
    CorePreparationPort, CorePreparedBatch, CorePreparedBatchBuilder, CorePreparedCapture,
    CoreRouteByteLease, CoreRouteResourceError, CoreRouteResourceKind, CoreRouteResources,
    CORE_RECORD_BATCH_MAX_RECORDS,
};
use ctx_history_core::{CoreRecord, SourceKey};
use ctx_history_index::{
    BaseEventIdentityLookup, CoreRecordPreparer, IndexError, PreparedCoreRecord,
    PreparedCoreRecordDraft, PreparedCoreRecordMaterialization,
};
use uuid::Uuid;

use super::{SourceBackedRouteError, SourceBackedRouteErrorKind};

/// Capture-local adapter for the index-owned immutable base identity view.
///
/// This is deliberately a transparent compile-time boundary: capture callers
/// keep the concrete type, while the index remains the sole lookup authority.
#[repr(transparent)]
#[derive(Clone)]
pub(crate) struct IndexBaseEventLookup(BaseEventIdentityLookup);

impl From<BaseEventIdentityLookup> for IndexBaseEventLookup {
    fn from(lookup: BaseEventIdentityLookup) -> Self {
        Self(lookup)
    }
}

impl BaseEventLookup for IndexBaseEventLookup {
    type Error = IndexError;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
        self.0.contains(event_id)
    }
}

/// Transparent capture adapter for the index-owned preparation authority.
///
/// All preparation remains static and concrete: the runtime envelope sees the
/// port type as a generic parameter and never erases this index value.
#[repr(transparent)]
#[derive(Clone)]
pub(crate) struct IndexCorePreparation(CoreRecordPreparer);

impl std::fmt::Debug for IndexCorePreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IndexCorePreparation(..)")
    }
}

impl From<CoreRecordPreparer> for IndexCorePreparation {
    fn from(preparer: CoreRecordPreparer) -> Self {
        Self(preparer)
    }
}

impl CorePreparationPort for IndexCorePreparation {
    type Prepared = PreparedCoreRecord;
    type Draft = PreparedCoreRecordDraft;
    type Failure = IndexError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
        self.0.prepare(record)
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
        self.0.prepare_draft(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        Ok(match draft.materialize(maximum_encoded_bytes)? {
            PreparedCoreRecordMaterialization::Prepared(prepared) => {
                CoreMaterialization::Prepared(prepared)
            }
            PreparedCoreRecordMaterialization::CapacityExceeded(draft) => {
                CoreMaterialization::CapacityExceeded(draft)
            }
        })
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        prepared.source()
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_core_bytes()
    }

    fn failure_kind(&self, failure: &Self::Failure) -> CorePreparationFailureKind {
        index_preparation_failure_kind(failure)
    }
}

fn index_preparation_failure_kind(failure: &IndexError) -> CorePreparationFailureKind {
    if matches!(
        failure,
        IndexError::ProjectionContract(_)
            | IndexError::CoreRecord(_)
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::EmptyDocumentField { .. }
            | IndexError::DocumentFieldTooLarge { .. }
    ) {
        CorePreparationFailureKind::InvalidSource
    } else {
        CorePreparationFailureKind::Internal
    }
}

pub(crate) type SourceBackedRouteResources = CoreRouteResources;
pub(crate) type SourceBackedRouteResourceKind = CoreRouteResourceKind;
pub(crate) type SourceBackedRouteByteReservation = CoreRouteByteLease;
pub(crate) type CoreRecordEmission = CorePreparedCapture<IndexCorePreparation>;
pub(crate) type CoreRecordEmissionBatchBuilder = CorePreparedBatchBuilder<IndexCorePreparation>;
pub(crate) type CoreRecordEmissionBatch = CorePreparedBatch<IndexCorePreparation>;
pub(crate) const SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS: usize = CORE_RECORD_BATCH_MAX_RECORDS;

impl From<CoreRouteResourceError> for SourceBackedRouteError {
    fn from(error: CoreRouteResourceError) -> Self {
        Self::new(
            SourceBackedRouteErrorKind::ResourceUnavailable,
            error.to_string(),
        )
    }
}

impl From<CorePreparationError<IndexError>> for SourceBackedRouteError {
    fn from(error: CorePreparationError<IndexError>) -> Self {
        match error {
            CorePreparationError::Preparation { kind, failure } => Self::new(
                match kind {
                    CorePreparationFailureKind::InvalidSource => {
                        SourceBackedRouteErrorKind::InvalidSource
                    }
                    CorePreparationFailureKind::Internal => SourceBackedRouteErrorKind::Internal,
                },
                failure.to_string(),
            ),
            CorePreparationError::Resource(error) => error.into(),
            CorePreparationError::Internal(detail) => {
                Self::new(SourceBackedRouteErrorKind::Internal, detail)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_capture_runtime::CorePreparationPort;
    use ctx_history_core::{
        derive_event_id, derive_session_id, CoreRecordError, EventIdentityInput, NativeItemKey,
        NativeSessionKey, ProjectionContractError, SessionIdentityInput, SourceAnchor, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, WriterOptions};

    #[test]
    fn index_preparation_classifies_only_source_contract_failures_as_invalid() {
        let invalid_source = [
            IndexError::ProjectionContract(ProjectionContractError::SourceChanged),
            IndexError::CoreRecord(CoreRecordError::UnsupportedVersion(0)),
            IndexError::CoreRecordPolicyRevisionMismatch {
                normalization: 0,
                expected_normalization: 1,
                content: 0,
                expected_content: 1,
            },
            IndexError::EmptyDocumentField { field: "body" },
            IndexError::DocumentFieldTooLarge {
                field: "body",
                actual: 2,
                maximum: 1,
            },
        ];
        for failure in invalid_source {
            assert_eq!(
                index_preparation_failure_kind(&failure),
                CorePreparationFailureKind::InvalidSource
            );
        }
        assert_eq!(
            index_preparation_failure_kind(&IndexError::ConcurrentGenerationChange),
            CorePreparationFailureKind::Internal
        );
    }

    #[test]
    fn index_preparation_delegates_exact_size_and_capacity_without_reencoding() {
        let temporary = crate::test_support_paths::tempdir().unwrap();
        let writer = GenerationWriter::open(
            temporary.path(),
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: 15_000_000,
            },
        )
        .unwrap()
        .into_writer()
        .unwrap();
        let preparation = IndexCorePreparation::from(writer.core_record_preparer());

        let direct = preparation.0.prepare(adapter_test_record()).unwrap();
        let prepared = preparation.prepare(adapter_test_record()).unwrap();
        assert_eq!(
            preparation.encoded_bytes(&prepared),
            direct.encoded_core_bytes(),
            "the runtime envelope must account for the preparer's exact final bytes"
        );

        let exact_bytes = preparation.encoded_bytes(&prepared);
        let draft = preparation.prepare_draft(adapter_test_record()).unwrap();
        assert!(matches!(
            preparation.materialize_draft(draft, exact_bytes.saturating_sub(1)),
            Ok(CoreMaterialization::CapacityExceeded(_))
        ));

        let draft = preparation.prepare_draft(adapter_test_record()).unwrap();
        let CoreMaterialization::Prepared(materialized) =
            preparation.materialize_draft(draft, exact_bytes).unwrap()
        else {
            panic!("the exact prepared size must admit materialization");
        };
        assert_eq!(preparation.encoded_bytes(&materialized), exact_bytes);
    }

    fn adapter_test_record() -> CoreRecord {
        let source = SourceKey::derive(
            "runtime-adapter-test",
            "runtime_adapter_fixture",
            "runtime-adapter-fixture-v1",
            1,
            SourceAnchor::CatalogLineage([7; 32]),
        )
        .unwrap();
        let native_session_key =
            NativeSessionKey::native_id("runtime-adapter.session", TypedKey::U64(1)).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "runtime-adapter-session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key =
            NativeItemKey::native_id("runtime-adapter.event", TypedKey::U64(1)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "runtime-adapter-event",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            source,
            1,
            "message",
            "primary",
            true,
            "runtime-adapter-parser-v1".to_owned(),
            "runtime adapter Core record".to_owned(),
        )
        .unwrap()
    }
}
