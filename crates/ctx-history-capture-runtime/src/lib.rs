//! Runtime-neutral contracts used by capture implementations.
//!
//! This crate intentionally owns no provider, source, JSONL, or index
//! implementation. Capture-side adapters select concrete lookup and Core
//! preparation types at compile time, so this boundary adds neither dynamic
//! dispatch nor storage.

use std::{
    error::Error,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use ctx_history_core::{CoreRecord, SourceKey};
use uuid::Uuid;

/// Looks up exact event identities from an immutable capture base.
pub trait BaseEventLookup: Clone + Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error>;
}

/// Classifies a concrete preparation failure without importing its authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorePreparationFailureKind {
    InvalidSource,
    Internal,
}

/// The result of admitting a prepared draft to an exact byte capacity.
///
/// A returned draft is boxed only on the uncommon capacity-exceeded path so
/// the normal prepared transport remains one contiguous `Vec` allocation.
#[derive(Debug)]
pub enum CoreMaterialization<P, D> {
    Prepared(P),
    CapacityExceeded(Box<D>),
}

/// Static bridge to a concrete Core-record preparation authority.
pub trait CorePreparationPort: Clone + Send + Sync + 'static {
    type Prepared: Send + 'static;
    type Draft: Send + 'static;
    type Failure: Error + Send + Sync + 'static;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure>;

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure>;

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure>;

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey;

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize;

    fn failure_kind(&self, failure: &Self::Failure) -> CorePreparationFailureKind;
}

/// Prepared Core records may retain at most this many exact encoded bytes
/// while crossing a shared worker-to-writer route envelope. Reservations are
/// live, not cumulative: large routes continue streaming after the writer
/// consumes each bounded emission.
pub const CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// A protocol emission carries at most one ordinary JSONL/Codex page worth of
/// Core records. Fan-out projectors split additional batches at this bound.
pub const CORE_RECORD_BATCH_MAX_RECORDS: usize = 64;

/// Replacement-document workers may each stage one provider-bounded logical
/// snapshot. The common document family uses at most four such workers and a
/// 256 MiB per-snapshot bound, making this an explicit aggregate ceiling.
pub const CORE_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES: u64 = 4 * 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreRouteResourceKind {
    CoreOutput,
    LogicalSourceScratch,
}

impl std::fmt::Display for CoreRouteResourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreOutput => "live prepared Core-record output",
            Self::LogicalSourceScratch => "physical logical-source scratch",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRouteResourceError {
    AccountingOverflow {
        kind: CoreRouteResourceKind,
        maximum: u64,
    },
    Unavailable {
        kind: CoreRouteResourceKind,
        maximum: u64,
        observed: u64,
    },
}

impl std::fmt::Display for CoreRouteResourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccountingOverflow { kind, maximum } => write!(
                formatter,
                "shared route {kind} byte accounting overflowed (maximum {maximum})"
            ),
            Self::Unavailable {
                kind,
                maximum,
                observed,
            } => write!(
                formatter,
                "shared route {kind} byte limit exceeded: maximum {maximum}, observed {observed}"
            ),
        }
    }
}

#[derive(Debug)]
pub enum CorePreparationError<E> {
    Preparation {
        kind: CorePreparationFailureKind,
        failure: E,
    },
    Resource(CoreRouteResourceError),
    Internal(&'static str),
}

impl<E: std::fmt::Display> std::fmt::Display for CorePreparationError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparation { failure, .. } => failure.fmt(formatter),
            Self::Resource(error) => error.fmt(formatter),
            Self::Internal(detail) => formatter.write_str(detail),
        }
    }
}

#[derive(Debug)]
struct CoreRouteByteBudget {
    maximum: u64,
    live: AtomicU64,
}

/// Cloneable resources shared by every scanner worker. Cloning this value
/// never creates another output or physical-scratch allowance.
#[derive(Debug, Clone)]
pub struct CoreRouteResources {
    leaf_worker_budget: usize,
    output: Arc<CoreRouteByteBudget>,
    scratch: Arc<CoreRouteByteBudget>,
}

impl CoreRouteResources {
    pub fn production(leaf_worker_budget: usize) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES,
            CORE_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES,
        )
    }

    fn with_byte_limits(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self {
            leaf_worker_budget: leaf_worker_budget.max(1),
            output: Arc::new(CoreRouteByteBudget {
                maximum: maximum_live_output_bytes,
                live: AtomicU64::new(0),
            }),
            scratch: Arc::new(CoreRouteByteBudget {
                maximum: maximum_physical_scratch_bytes,
                live: AtomicU64::new(0),
            }),
        }
    }

    pub fn for_test(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            maximum_live_output_bytes,
            maximum_physical_scratch_bytes,
        )
    }

    pub fn leaf_worker_budget(&self) -> usize {
        self.leaf_worker_budget
    }

    pub fn maximum_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        match kind {
            CoreRouteResourceKind::CoreOutput => self.output.maximum,
            CoreRouteResourceKind::LogicalSourceScratch => self.scratch.maximum,
        }
    }

    pub fn core_output_batch_reservation_bytes(&self) -> u64 {
        if self.output.maximum == 0 {
            return 0;
        }
        let workers = u64::try_from(self.leaf_worker_budget).unwrap_or(u64::MAX);
        self.output.maximum.checked_div(workers).unwrap_or(0).max(1)
    }

    pub fn reserve(
        &self,
        kind: CoreRouteResourceKind,
        bytes: usize,
    ) -> Result<CoreRouteByteLease, CoreRouteResourceError> {
        let budget = match kind {
            CoreRouteResourceKind::CoreOutput => &self.output,
            CoreRouteResourceKind::LogicalSourceScratch => &self.scratch,
        };
        let bytes =
            u64::try_from(bytes).map_err(|_| CoreRouteResourceError::AccountingOverflow {
                kind,
                maximum: budget.maximum,
            })?;
        let mut live = budget.live.load(Ordering::Acquire);
        loop {
            let Some(next) = live.checked_add(bytes) else {
                return Err(CoreRouteResourceError::AccountingOverflow {
                    kind,
                    maximum: budget.maximum,
                });
            };
            if next > budget.maximum {
                return Err(CoreRouteResourceError::Unavailable {
                    kind,
                    maximum: budget.maximum,
                    observed: next,
                });
            }
            match budget
                .live
                .compare_exchange_weak(live, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(CoreRouteByteLease {
                        budget: Arc::clone(budget),
                        bytes,
                    });
                }
                Err(actual) => live = actual,
            }
        }
    }

    pub fn live_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        match kind {
            CoreRouteResourceKind::CoreOutput => &self.output,
            CoreRouteResourceKind::LogicalSourceScratch => &self.scratch,
        }
        .live
        .load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct CoreRouteByteLease {
    budget: Arc<CoreRouteByteBudget>,
    bytes: u64,
}

impl CoreRouteByteLease {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for CoreRouteByteLease {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.budget.live.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

/// A prepared Core record held together with its exact live-byte lease.
pub struct CorePreparedCapture<P: CorePreparationPort> {
    prepared: P::Prepared,
    lease: CoreRouteByteLease,
}

impl<P: CorePreparationPort> std::fmt::Debug for CorePreparedCapture<P>
where
    P::Prepared: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CorePreparedCapture")
            .field("prepared", &self.prepared)
            .field("reserved_bytes", &self.lease.bytes())
            .finish()
    }
}

impl<P: CorePreparationPort> CorePreparedCapture<P> {
    pub fn new(
        record: CoreRecord,
        resources: &CoreRouteResources,
        port: &P,
    ) -> Result<Self, CorePreparationError<P::Failure>> {
        let prepared = Self::prepare(record, port)?;
        Self::from_prepared(prepared, resources, port)
    }

    pub fn prepare(
        record: CoreRecord,
        port: &P,
    ) -> Result<P::Prepared, CorePreparationError<P::Failure>> {
        port.prepare(record)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    pub fn prepare_draft(
        record: CoreRecord,
        port: &P,
    ) -> Result<P::Draft, CorePreparationError<P::Failure>> {
        port.prepare_draft(record)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    pub fn materialize_draft(
        draft: P::Draft,
        maximum_encoded_bytes: usize,
        port: &P,
    ) -> Result<CoreMaterialization<P::Prepared, P::Draft>, CorePreparationError<P::Failure>> {
        port.materialize_draft(draft, maximum_encoded_bytes)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    pub fn from_prepared(
        prepared: P::Prepared,
        resources: &CoreRouteResources,
        port: &P,
    ) -> Result<Self, CorePreparationError<P::Failure>> {
        let lease = resources
            .reserve(
                CoreRouteResourceKind::CoreOutput,
                port.encoded_bytes(&prepared),
            )
            .map_err(CorePreparationError::Resource)?;
        Ok(Self { prepared, lease })
    }

    pub fn into_prepared(self) -> (P::Prepared, CoreRouteByteLease) {
        let Self { prepared, lease } = self;
        (prepared, lease)
    }
}

/// A mutable worker-local Core-record batch with one shared output lease.
#[derive(Debug)]
pub struct CorePreparedBatchBuilder<P: CorePreparationPort> {
    prepared: Vec<P::Prepared>,
    lease: Option<CoreRouteByteLease>,
    prepared_bytes: u64,
}

impl<P: CorePreparationPort> Default for CorePreparedBatchBuilder<P> {
    fn default() -> Self {
        Self {
            prepared: Vec::new(),
            lease: None,
            prepared_bytes: 0,
        }
    }
}

impl<P: CorePreparationPort> CorePreparedBatchBuilder<P> {
    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    pub fn has_lease(&self) -> bool {
        self.lease.is_some()
    }

    pub fn lease_bytes(&self) -> u64 {
        self.lease.as_ref().map_or(0, CoreRouteByteLease::bytes)
    }

    pub fn remaining_bytes(&self) -> u64 {
        self.lease_bytes().saturating_sub(self.prepared_bytes)
    }

    pub fn can_admit(&self, prepared_bytes: u64) -> bool {
        self.lease.as_ref().is_some_and(|lease| {
            self.prepared_bytes
                .checked_add(prepared_bytes)
                .is_some_and(|total| total <= lease.bytes())
        })
    }

    pub fn reserve_bytes(
        &mut self,
        reservation_bytes: u64,
        resources: &CoreRouteResources,
    ) -> Result<(), CorePreparationError<P::Failure>> {
        debug_assert!(self.is_empty());
        debug_assert!(self.lease.is_none());
        let reservation_bytes = usize::try_from(reservation_bytes).map_err(|_| {
            CorePreparationError::Resource(CoreRouteResourceError::AccountingOverflow {
                kind: CoreRouteResourceKind::CoreOutput,
                maximum: resources.maximum_bytes(CoreRouteResourceKind::CoreOutput),
            })
        })?;
        self.lease = Some(
            resources
                .reserve(CoreRouteResourceKind::CoreOutput, reservation_bytes)
                .map_err(CorePreparationError::Resource)?,
        );
        Ok(())
    }

    pub fn release_empty_lease(&mut self) {
        debug_assert!(self.is_empty());
        self.lease = None;
        self.prepared_bytes = 0;
    }

    pub fn push(
        &mut self,
        prepared: P::Prepared,
        port: &P,
    ) -> Result<(), CorePreparationError<P::Failure>> {
        let prepared_bytes = u64::try_from(port.encoded_bytes(&prepared)).map_err(|_| {
            CorePreparationError::Internal("prepared Core-record byte count overflowed")
        })?;
        if !self.can_admit(prepared_bytes) {
            return Err(CorePreparationError::Internal(
                "prepared Core record exceeded its batch reservation",
            ));
        }
        self.prepared_bytes = self.prepared_bytes.checked_add(prepared_bytes).ok_or(
            CorePreparationError::Internal("prepared Core-record batch byte count overflowed"),
        )?;
        self.prepared.push(prepared);
        Ok(())
    }

    pub fn take_batch(
        &mut self,
    ) -> Result<Option<CorePreparedBatch<P>>, CorePreparationError<P::Failure>> {
        if self.is_empty() {
            self.lease = None;
            self.prepared_bytes = 0;
            return Ok(None);
        }
        if self.prepared.len() > CORE_RECORD_BATCH_MAX_RECORDS {
            return Err(CorePreparationError::Internal(
                "Core-record emission batch exceeds the shared protocol bound",
            ));
        }
        let lease = self.lease.take().ok_or(CorePreparationError::Internal(
            "prepared Core-record batch has no live byte reservation",
        ))?;
        self.prepared_bytes = 0;
        Ok(Some(CorePreparedBatch {
            prepared: std::mem::take(&mut self.prepared),
            lease,
        }))
    }
}

/// One worker-prepared Core-record batch. Its lease is retained until a
/// writer has accepted all records or the batch is dropped on an error path.
#[derive(Debug)]
pub struct CorePreparedBatch<P: CorePreparationPort> {
    prepared: Vec<P::Prepared>,
    lease: CoreRouteByteLease,
}

impl<P: CorePreparationPort> CorePreparedBatch<P> {
    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &P::Prepared> {
        self.prepared.iter()
    }

    pub fn into_prepared(self) -> (Vec<P::Prepared>, CoreRouteByteLease) {
        (self.prepared, self.lease)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeLookup {
        event_ids: HashSet<Uuid>,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("fake lookup failed")]
    struct FakeLookupError;

    #[derive(Clone)]
    struct FakePreparationPort;

    #[derive(Debug)]
    struct FakePrepared {
        encoded_bytes: usize,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("fake preparation failed")]
    struct FakePreparationError;

    impl CorePreparationPort for FakePreparationPort {
        type Prepared = FakePrepared;
        type Draft = ();
        type Failure = FakePreparationError;

        fn prepare(&self, _record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
            Err(FakePreparationError)
        }

        fn prepare_draft(&self, _record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
            Err(FakePreparationError)
        }

        fn materialize_draft(
            &self,
            _draft: Self::Draft,
            _maximum_encoded_bytes: usize,
        ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
            Err(FakePreparationError)
        }

        fn prepared_source<'a>(&self, _prepared: &'a Self::Prepared) -> &'a SourceKey {
            panic!("the fake batch test does not inspect source identity")
        }

        fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
            prepared.encoded_bytes
        }

        fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
            CorePreparationFailureKind::InvalidSource
        }
    }

    impl BaseEventLookup for FakeLookup {
        type Error = FakeLookupError;

        fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
            Ok(self.event_ids.contains(&event_id))
        }
    }

    fn lookup_contains<L: BaseEventLookup>(lookup: &L, event_id: Uuid) -> Result<bool, L::Error> {
        lookup.contains(event_id)
    }

    #[test]
    fn fake_lookup_is_static_generic_and_exact() {
        let present = Uuid::new_v4();
        let absent = Uuid::new_v4();
        let lookup = FakeLookup {
            event_ids: HashSet::from([present]),
        };

        assert!(lookup_contains(&lookup, present).unwrap());
        assert!(!lookup_contains(&lookup, absent).unwrap());
    }

    #[test]
    fn route_output_budget_is_live_without_a_cumulative_cap() {
        let resources = CoreRouteResources::for_test(2, 9, 20);
        let first = resources
            .reserve(CoreRouteResourceKind::CoreOutput, 5)
            .unwrap();
        assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 5);
        drop(first);
        let second = resources
            .reserve(CoreRouteResourceKind::CoreOutput, 5)
            .unwrap();
        drop(second);
        assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
    }

    #[test]
    fn cloned_workers_share_one_live_output_budget_exactly_one_over() {
        let resources = CoreRouteResources::for_test(4, 9, 20);
        let first = resources
            .reserve(CoreRouteResourceKind::CoreOutput, 5)
            .unwrap();
        let error = resources
            .clone()
            .reserve(CoreRouteResourceKind::CoreOutput, 5)
            .unwrap_err();
        assert_eq!(
            error,
            CoreRouteResourceError::Unavailable {
                kind: CoreRouteResourceKind::CoreOutput,
                maximum: 9,
                observed: 10,
            }
        );
        drop(first);
        resources
            .reserve(CoreRouteResourceKind::CoreOutput, 5)
            .unwrap();
    }

    #[test]
    fn physical_scratch_has_a_separate_exact_aggregate_limit() {
        let resources = CoreRouteResources::for_test(4, 3, 9);
        let first = resources
            .reserve(CoreRouteResourceKind::LogicalSourceScratch, 5)
            .unwrap();
        let error = resources
            .clone()
            .reserve(CoreRouteResourceKind::LogicalSourceScratch, 5)
            .unwrap_err();
        assert_eq!(
            error,
            CoreRouteResourceError::Unavailable {
                kind: CoreRouteResourceKind::LogicalSourceScratch,
                maximum: 9,
                observed: 10,
            }
        );
        assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
        drop(first);
        assert_eq!(
            resources.live_bytes(CoreRouteResourceKind::LogicalSourceScratch),
            0
        );
    }

    #[test]
    fn generic_batch_uses_one_vec_and_releases_its_shared_lease() {
        let resources = CoreRouteResources::for_test(1, 9, 1);
        let port = FakePreparationPort;
        let mut builder = CorePreparedBatchBuilder::<FakePreparationPort>::default();
        builder.reserve_bytes(9, &resources).unwrap();
        builder
            .push(FakePrepared { encoded_bytes: 4 }, &port)
            .unwrap();
        builder
            .push(FakePrepared { encoded_bytes: 5 }, &port)
            .unwrap();
        assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 9);

        let batch = builder.take_batch().unwrap().unwrap();
        assert_eq!(batch.len(), 2);
        drop(batch);
        assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
    }
}
