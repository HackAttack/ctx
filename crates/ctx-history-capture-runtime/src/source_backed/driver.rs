use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    path::PathBuf,
    sync::Arc,
};

use ctx_history_capture_model::{SourceBackedRecordProgressDelta, SourceRouteIdentity};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, SourceKey,
};
use thiserror::Error;

use crate::{
    CaptureLifecycleSink, CorePreparationError, CorePreparationFailureKind, CorePreparedBatch,
    CorePreparedBatchBuilder, CorePreparedCapture, CoreRecordProgress, CoreRouteResourceError,
    ImmutableCaptureSnapshot, CORE_RECORD_BATCH_MAX_RECORDS,
};

use super::{
    diagnostics::{self, *},
    SourceBackedCertifiedRemoval, SourceBackedCurrentSourceProgress, SourceBackedRouteResources,
};

pub const MAX_RECORDED_SOURCE_BACKED_FAILURES: usize = 64;
pub const MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES: usize = 512;
pub const MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES: usize = 512;
pub const MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS: usize = 64;
pub const MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES: usize = 4 * 1024;
pub const MAX_SOURCE_BACKED_REJECTION_PAYLOAD_TYPE_BYTES: usize = 128;

pub type SourceBackedCoordinatorResult<T, E> = Result<T, SourceBackedCoordinatorError<E>>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

// Bounded route, source, and record diagnostics live in the diagnostics module.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedDeletionDisposition {
    Deferred,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteErrorKind {
    Unavailable,
    SourceChanged,
    InvalidSource,
    Unsupported,
    ResourceUnavailable,
    Internal,
}

impl SourceBackedRouteErrorKind {
    pub fn source_failure_class(self) -> Option<SourceBackedSourceFailureClass> {
        match self {
            Self::Unavailable => Some(SourceBackedSourceFailureClass::Unavailable),
            Self::SourceChanged => Some(SourceBackedSourceFailureClass::SourceChanged),
            Self::InvalidSource => Some(SourceBackedSourceFailureClass::Unreadable),
            Self::Unsupported => Some(SourceBackedSourceFailureClass::Incompatible),
            Self::ResourceUnavailable | Self::Internal => None,
        }
    }

    /// Only these failures are narrow enough for a family with a complete,
    /// stable inventory and exact source ownership to retain one source while
    /// publishing certified peers. Source drift, schema ambiguity, aggregate
    /// resource failure, and internal failures remain route-fatal.
    pub const fn is_logical_source_failure(self) -> bool {
        matches!(self, Self::Unavailable | Self::InvalidSource)
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {detail}")]
pub struct SourceBackedRouteError {
    pub kind: SourceBackedRouteErrorKind,
    pub detail: String,
}

impl SourceBackedRouteError {
    pub fn new(kind: SourceBackedRouteErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl From<CoreRouteResourceError> for SourceBackedRouteError {
    fn from(error: CoreRouteResourceError) -> Self {
        Self::new(
            SourceBackedRouteErrorKind::ResourceUnavailable,
            error.to_string(),
        )
    }
}

impl<E: fmt::Display> From<CorePreparationError<E>> for SourceBackedRouteError {
    fn from(error: CorePreparationError<E>) -> Self {
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

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Index(#[from] E),
    #[error(
        "predecessor generation migration committed successor {generation_id}, but writer recovery is still required: {detail}"
    )]
    CommittedPredecessorMigrationRecovery {
        generation_id: String,
        detail: String,
    },
    #[error("invalid source-backed route for {provider}: {detail}")]
    InvalidRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source-backed scan failed for {provider}: {source}")]
    RouteScan {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source-backed refresh has an unknown or unavailable route for {provider}: {detail}")]
    UnavailableRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source {source_id} was staged by more than one provider route")]
    DuplicateSourceOwner { source_id: String },
    #[error("base source {source_id} was not claimed by any provider route in this refresh")]
    UnclaimedBaseSource { source_id: String },
    #[error("source deletion was not certified by its supplied authoritative inventory")]
    InvalidDeletionWitness,
    #[error("retained source deletion {source_id} could not be recertified: {detail}")]
    RetainedDeletionRecertification { source_id: String, detail: String },
    #[error("source-backed refresh progress callback failed: {0}")]
    Progress(SourceBackedRouteError),
    #[error("source-backed Core-record emission failed: {0}")]
    CoreEmission(SourceBackedRouteError),
    #[error("logical-source outcome is inconsistent: {detail}")]
    InvalidLogicalSourceFailure { detail: &'static str },
    #[error("selected source-backed route {route_id} is unknown or not executable")]
    InvalidRefreshScope { route_id: String },
    #[error(
        "source-backed refresh completed with source failures but retained no usable source: {failed_routes}"
    )]
    NoUsableSourceRoutes {
        failed_routes: SourceBackedSourceFailures,
    },
    #[error(
        "source-backed refresh completed with logical-source failures but retained no usable source"
    )]
    NoUsableLogicalSources {
        failed_sources: SourceBackedLogicalSourceFailures,
    },
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer, L: CaptureLifecycleSink> {
    pub lifecycle: &'writer mut L,
    pub core_record_preparer: L::Preparation,
    pub owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
    pub route_index: usize,
    pub route_identity: SourceRouteIdentity,
    pub base_route_control: Option<Vec<u8>>,
    pub resources: SourceBackedRouteResources,
    pub logical_source_failures: &'writer mut SourceBackedLogicalSourceFailures,
    pub record_rejections: &'writer mut SourceBackedRecordRejections,
    pub record_progress: Option<
        &'writer mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<(), L::Error>,
    >,
    pub current_source_progress: Option<
        &'writer mut dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>,
    >,
    pub last_progress_session_id: Option<[u8; 32]>,
}

impl<L: CaptureLifecycleSink> SourceBackedGenerationSink<'_, L> {
    #[allow(clippy::too_many_arguments)]
    pub fn new<'writer>(
        lifecycle: &'writer mut L,
        owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
        complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
        applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
        route_index: usize,
        route_identity: SourceRouteIdentity,
        base_route_control: Option<Vec<u8>>,
        resources: SourceBackedRouteResources,
        logical_source_failures: &'writer mut SourceBackedLogicalSourceFailures,
        record_rejections: &'writer mut SourceBackedRecordRejections,
        record_progress: Option<
            &'writer mut dyn FnMut(
                SourceBackedRecordProgressDelta,
            ) -> SourceBackedCoordinatorResult<(), L::Error>,
        >,
        current_source_progress: Option<
            &'writer mut dyn FnMut(
                SourceBackedCurrentSourceProgress,
            ) -> SourceBackedRouteResult<()>,
        >,
        last_progress_session_id: Option<[u8; 32]>,
    ) -> SourceBackedGenerationSink<'writer, L> {
        let core_record_preparer = lifecycle.core_preparation();
        SourceBackedGenerationSink {
            lifecycle,
            core_record_preparer,
            owners,
            complete_inventories,
            applied_removals,
            route_index,
            route_identity,
            base_route_control,
            resources,
            logical_source_failures,
            record_rejections,
            record_progress,
            current_source_progress,
            last_progress_session_id,
        }
    }

    pub fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.resources.reconciliation_demand()
    }

    pub fn base_route_control(&self) -> Option<&[u8]> {
        self.base_route_control.as_deref()
    }

    pub fn route_identity(&self) -> &SourceRouteIdentity {
        &self.route_identity
    }

    pub fn base_snapshot(&self) -> Option<L::Snapshot<'_>> {
        self.lifecycle.base_snapshot()
    }

    /// Carries unmentioned members of this exact route from the locked Core
    /// base while changed members are replaced atomically.
    pub fn retain_unstaged_base_route_sources(
        &mut self,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.lifecycle
            .retain_unstaged_route_members(&self.route_identity)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SourceOwner {
    pub route_index: usize,
    pub source: SourceKey,
    pub present: bool,
    pub revalidation: Option<SourceBackedRouteRevalidation>,
}

impl SourceOwner {
    pub fn new(
        route_index: usize,
        source: SourceKey,
        present: bool,
        revalidation: Option<SourceBackedRouteRevalidation>,
    ) -> Self {
        Self {
            route_index,
            source,
            present,
            revalidation,
        }
    }

    pub fn route_index(&self) -> usize {
        self.route_index
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn is_present(&self) -> bool {
        self.present
    }

    pub fn revalidation(&self) -> Option<&SourceBackedRouteRevalidation> {
        self.revalidation.as_ref()
    }
}

#[derive(Clone)]
pub enum SourceBackedRouteRevalidation {
    Source(CertifiedSource),
    Deletion(CertifiedSourceDeletion),
}

#[derive(Clone)]
pub struct CompleteInventoryOwner {
    pub route_index: usize,
    pub inventory: CertifiedSourceInventory,
}

impl CompleteInventoryOwner {
    pub fn new(route_index: usize, inventory: CertifiedSourceInventory) -> Self {
        Self {
            route_index,
            inventory,
        }
    }

    pub fn route_index(&self) -> usize {
        self.route_index
    }

    pub fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }
}

type CoreRecordEmission<L> = CorePreparedCapture<<L as CaptureLifecycleSink>::Preparation>;
pub type CoreRecordEmissionBatch<L> = CorePreparedBatch<<L as CaptureLifecycleSink>::Preparation>;
pub type CoreRecordEmissionBatchBuilder<L> =
    CorePreparedBatchBuilder<<L as CaptureLifecycleSink>::Preparation>;
pub const SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS: usize = CORE_RECORD_BATCH_MAX_RECORDS;

impl<L: CaptureLifecycleSink> SourceBackedGenerationSink<'_, L> {
    /// Returns the capture-facing lookup pinned to this writer's base generation.
    pub fn base_event_lookup(&self) -> L::BaseLookup {
        self.lifecycle.base_event_lookup()
    }

    pub fn route_resources(&self) -> SourceBackedRouteResources {
        self.resources.clone()
    }

    pub fn report_current_source_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        self.current_source_progress
            .as_mut()
            .map_or(Ok(()), |report| report(progress))
    }

    pub fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.lifecycle.base_source(source)
    }

    pub fn pinned_append_base(&self, source: &SourceKey) -> Option<L::PinnedAppendBase> {
        self.lifecycle
            .pinned_append_base(&self.route_identity, source)
    }

    /// Returns only the prior certified sources retained by this route. A
    /// provider route must not infer ownership from the provider family alone:
    /// another retained route may intentionally cover the same input tree.
    pub fn base_route_sources(
        &self,
    ) -> SourceBackedCoordinatorResult<HashMap<SourceKey, CertifiedSource>, L::Error> {
        let Some(snapshot) = self.lifecycle.base_snapshot() else {
            return Ok(HashMap::new());
        };
        let Some(route) = snapshot.source_route(&self.route_identity) else {
            return Ok(HashMap::new());
        };
        let mut sources = HashMap::with_capacity(route.sources().len());
        for source in route.sources() {
            let certificate = snapshot
                .sources()
                .iter()
                .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
                .cloned()
                .ok_or_else(|| {
                    L::invariant_error("source-route snapshot names a missing certified source")
                })?;
            sources.insert(source.clone(), certificate);
        }
        Ok(sources)
    }

    /// Whether an exact source is retained or has already been claimed by a
    /// different route in this refresh. Such a source is outside this route's
    /// mutation authority even when its selected filesystem root overlaps.
    pub fn source_owned_by_other_route(&self, source: &SourceKey) -> bool {
        let owned_in_attempt = self.owners.values().any(|owner| {
            owner.route_index != self.route_index && owner.source.exact_descriptor_eq(source)
        });
        owned_in_attempt
            || self.lifecycle.base_snapshot().is_some_and(|snapshot| {
                snapshot.source_routes().any(|route| {
                    route.route_identity() != &self.route_identity
                        && route
                            .sources()
                            .iter()
                            .any(|candidate| candidate.exact_descriptor_eq(source))
                })
            })
    }

    pub fn begin_source(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim_present(&source)?;
        self.lifecycle.begin_source_replace(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource, L::Error> {
        self.claim_present(&source)?;
        self.lifecycle
            .begin_source_append(source)
            .map_err(Into::into)
    }

    pub fn begin_source_append_from_base(
        &mut self,
        base: L::PinnedAppendBase,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource, L::Error> {
        let source = L::pinned_append_base_source(&base)
            .observation()
            .source()
            .clone();
        self.claim_present(&source)?;
        self.lifecycle
            .begin_source_append_from_base(base)
            .map_err(Into::into)
    }

    pub fn add_core_record(
        &mut self,
        record: CoreRecord,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let progress = CoreRecordProgress::from_record(&record);
        let emission =
            CoreRecordEmission::<L>::new(record, &self.resources, &self.core_record_preparer)
                .map_err(SourceBackedRouteError::from)
                .map_err(SourceBackedCoordinatorError::CoreEmission)?;
        self.accept_core_record_emission(emission)?;
        self.report_record_progress(
            1,
            0,
            std::slice::from_ref(&progress.session_id),
            progress.messages,
            progress.tool_calls,
        )
    }

    pub fn add_core_record_emission(
        &mut self,
        emission: CoreRecordEmission<L>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.accept_core_record_emission(emission)?;
        self.report_record_progress(1, 0, &[], 0, 0)
    }

    fn accept_core_record_emission(
        &mut self,
        emission: CoreRecordEmission<L>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let (prepared, reservation) = emission.into_prepared();
        self.lifecycle.add_prepared(prepared)?;
        drop(reservation);
        Ok(())
    }

    pub fn add_core_records_with_completed_bytes(
        &mut self,
        records: Vec<CoreRecord>,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let accepted_records = u64::try_from(records.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record page count overflowed",
            ))
        })?;
        for record in records {
            let emission =
                CoreRecordEmission::<L>::new(record, &self.resources, &self.core_record_preparer)
                    .map_err(SourceBackedRouteError::from)
                    .map_err(SourceBackedCoordinatorError::CoreEmission)?;
            self.accept_core_record_emission(emission)?;
        }
        self.report_record_progress(accepted_records, completed_bytes, &[], 0, 0)
    }

    pub fn add_core_record_emission_batch(
        &mut self,
        batch: CoreRecordEmissionBatch<L>,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let accepted_records = u64::try_from(batch.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record emission batch count overflowed",
            ))
        })?;
        let progress = batch.progress().clone();
        let (prepared_records, reservation) = batch.into_prepared();
        for prepared in prepared_records {
            self.lifecycle.add_prepared(prepared)?;
        }
        drop(reservation);
        self.report_record_progress(
            accepted_records,
            completed_bytes,
            &progress.session_ids,
            progress.messages,
            progress.tool_calls,
        )
    }

    pub fn record_logical_source_failure(
        &mut self,
        source: SourceKey,
        failure: SourceBackedRouteError,
        carried_forward: bool,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if !failure.kind.is_logical_source_failure() {
            return Err(SourceBackedCoordinatorError::InvalidLogicalSourceFailure {
                detail: "non-local failure was reported as a logical-source outcome",
            });
        }
        let class = failure.kind.source_failure_class().ok_or(
            SourceBackedCoordinatorError::InvalidLogicalSourceFailure {
                detail: "logical-source failure has no stable failure class",
            },
        )?;
        self.logical_source_failures
            .record(SourceBackedLogicalSourceFailure {
                route_index: self.route_index,
                route_identity: self.route_identity.clone(),
                source,
                class,
                carried_forward,
                detail: diagnostics::bounded_text(
                    &failure.detail,
                    MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
                ),
            });
        Ok(())
    }

    pub fn record_rejection(&mut self, rejection: SourceBackedRecordRejectionDraft) {
        self.record_rejections.record(SourceBackedRecordRejection {
            route_index: self.route_index,
            route_identity: self.route_identity.clone(),
            source: rejection.source,
            provider: rejection.provider,
            source_selector: diagnostics::bounded_text(
                &rejection.source_selector,
                MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES,
            ),
            line_number: rejection.line_number,
            payload_type: rejection.payload_type.map(|payload_type| {
                diagnostics::bounded_text(
                    &payload_type,
                    MAX_SOURCE_BACKED_REJECTION_PAYLOAD_TYPE_BYTES,
                )
            }),
            class: rejection.class,
            detail: diagnostics::bounded_text(
                &rejection.detail,
                MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
            ),
        });
    }

    pub fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        let (rejections, omitted) = rejections.into_parts();
        for rejection in rejections {
            self.record_rejection(rejection);
        }
        self.record_omitted_rejections(omitted);
    }

    pub fn record_omitted_rejections(&mut self, omitted: usize) {
        self.record_rejections.record_omitted(omitted);
    }

    pub fn report_completed_bytes(
        &mut self,
        bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.report_record_progress(0, bytes, &[], 0, 0)
    }

    fn report_record_progress(
        &mut self,
        accepted_records: u64,
        completed_bytes: u64,
        session_ids: &[[u8; 32]],
        messages: u64,
        tool_calls: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if let Some(report_progress) = self.record_progress.as_mut() {
            let mut session_transitions = Vec::new();
            for session_id in session_ids {
                if self.last_progress_session_id.as_ref() != Some(session_id) {
                    session_transitions.push(*session_id);
                    self.last_progress_session_id = Some(*session_id);
                }
            }
            report_progress(SourceBackedRecordProgressDelta {
                accepted_records,
                completed_bytes,
                session_ids: session_transitions,
                messages,
                tool_calls,
            })?;
        }
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let source = certificate.observation().source().clone();
        self.lifecycle.certify_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let certificate = append.current().clone();
        let source = certificate.observation().source().clone();
        self.lifecycle.certify_source_append(append)?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn retain_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim_present(certificate.observation().source())?;
        let source = certificate.observation().source().clone();
        self.lifecycle.retain_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.lifecycle
            .certify_complete_inventory(inventory.clone())?;
        self.complete_inventories.push(CompleteInventoryOwner {
            route_index: self.route_index,
            inventory,
        });
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<SourceBackedDeletionDisposition, L::Error> {
        if !deletion.verifies(&inventory) {
            return Err(SourceBackedCoordinatorError::InvalidDeletionWitness);
        }
        self.claim_absent(deletion.source())?;
        self.lifecycle
            .delete_source(deletion.clone(), inventory.clone())?;
        self.record_revalidation(
            deletion.source(),
            SourceBackedRouteRevalidation::Deletion(deletion.clone()),
        )?;
        self.applied_removals.push(SourceBackedCertifiedRemoval {
            deletion,
            inventory,
        });
        Ok(SourceBackedDeletionDisposition::Deleted)
    }

    pub fn replace_source(
        &mut self,
        certificate: CertifiedSource,
        core_records: impl IntoIterator<Item = CoreRecord>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.begin_source(certificate.observation().source().clone())?;
        for record in core_records {
            self.add_core_record(record)?;
        }
        self.certify_source(certificate)
    }

    pub fn claim_present(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim(source, true)
    }

    pub fn claim_absent(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim(source, false)
    }

    fn claim(
        &mut self,
        source: &SourceKey,
        present: bool,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let digest = source.identity().digest();
        match self.owners.get(&digest) {
            Some(owner)
                if owner.route_index != self.route_index
                    || !owner.source.exact_descriptor_eq(source) =>
            {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(owner) if owner.present != present => {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(_) => {}
            None => {
                self.owners.insert(
                    digest,
                    SourceOwner {
                        route_index: self.route_index,
                        source: source.clone(),
                        present,
                        revalidation: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn record_revalidation(
        &mut self,
        source: &SourceKey,
        revalidation: SourceBackedRouteRevalidation,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let owner = self
            .owners
            .get_mut(&source.identity().digest())
            .filter(|owner| {
                owner.route_index == self.route_index
                    && owner.source.exact_descriptor_eq(source)
                    && owner.revalidation.is_none()
            })
            .ok_or_else(|| L::invariant_error("source certification lost its route-local owner"))?;
        owner.revalidation = Some(revalidation);
        Ok(())
    }
}

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

pub type SourceBackedScanCallback<L> = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer, L>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
pub type SourcePredicate = dyn Fn(&SourceKey) -> SourceBackedRouteResult<bool> + Send + Sync;
pub type RevalidationCallback = dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> SourceBackedRouteResult<bool>
    + Send
    + Sync;
pub type CompleteInventoryRevalidationCallback =
    dyn Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool> + Send + Sync;
pub type SuccessfulPublicationCallback = dyn Fn() + Send + Sync;
pub type RoutePublicationRevalidationCallback = dyn Fn() -> bool + Send + Sync;
pub type RoutePublicationControlCallback =
    dyn Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync;
pub type WatchTargetsCallback = dyn Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync;

#[derive(Debug, Clone, Default)]
pub struct SourceBackedRouteWatchTargets {
    pub sqlite_databases: BTreeSet<PathBuf>,
    pub authority_paths: BTreeSet<PathBuf>,
}

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
pub struct SourceBackedRouteDriver<L: CaptureLifecycleSink, C> {
    pub scan: Arc<SourceBackedScanCallback<L>>,
    pub owns_source: Arc<SourcePredicate>,
    pub revalidate: Arc<RevalidationCallback>,
    pub revalidate_complete_inventory: Option<Arc<CompleteInventoryRevalidationCallback>>,
    pub after_successful_publication: Option<Arc<SuccessfulPublicationCallback>>,
    pub revalidate_at_publication: Option<Arc<RoutePublicationRevalidationCallback>>,
    pub publication_control: Option<Arc<RoutePublicationControlCallback>>,
    pub watch_targets: Option<Arc<WatchTargetsCallback>>,
    pub route_control_expectation: Option<C>,
    pub uses_parallel_leaf_workers: bool,
}

impl<L: CaptureLifecycleSink, C: Clone> Clone for SourceBackedRouteDriver<L, C> {
    fn clone(&self) -> Self {
        Self {
            scan: Arc::clone(&self.scan),
            owns_source: Arc::clone(&self.owns_source),
            revalidate: Arc::clone(&self.revalidate),
            revalidate_complete_inventory: self.revalidate_complete_inventory.clone(),
            after_successful_publication: self.after_successful_publication.clone(),
            revalidate_at_publication: self.revalidate_at_publication.clone(),
            publication_control: self.publication_control.clone(),
            watch_targets: self.watch_targets.clone(),
            route_control_expectation: self.route_control_expectation.clone(),
            uses_parallel_leaf_workers: self.uses_parallel_leaf_workers,
        }
    }
}

impl<L: CaptureLifecycleSink, C> fmt::Debug for SourceBackedRouteDriver<L, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl<L: CaptureLifecycleSink, C> SourceBackedRouteDriver<L, C> {
    pub fn new(
        scan: impl for<'writer> Fn(
                &mut SourceBackedGenerationSink<'writer, L>,
            ) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_fallible(
            scan,
            move |source| Ok(owns_source(source)),
            move |target| Ok(revalidate(target)),
        )
    }

    pub fn new_fallible(
        scan: impl for<'writer> Fn(
                &mut SourceBackedGenerationSink<'writer, L>,
            ) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> SourceBackedRouteResult<bool> + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> SourceBackedRouteResult<bool>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            scan: Arc::new(scan),
            owns_source: Arc::new(owns_source),
            revalidate: Arc::new(revalidate),
            revalidate_complete_inventory: None,
            after_successful_publication: None,
            revalidate_at_publication: None,
            publication_control: None,
            watch_targets: None,
            route_control_expectation: None,
            uses_parallel_leaf_workers: false,
        }
    }

    pub fn with_parallel_leaf_workers(mut self) -> Self {
        self.uses_parallel_leaf_workers = true;
        self
    }

    pub fn with_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_complete_inventory =
            Some(Arc::new(move |inventory| Ok(revalidate(inventory))));
        self
    }

    pub fn with_fallible_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.revalidate_complete_inventory = Some(Arc::new(revalidate));
        self
    }

    pub fn with_publication_revalidation(
        mut self,
        revalidate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_at_publication = Some(Arc::new(revalidate));
        self
    }

    pub fn with_publication_control(
        mut self,
        control: impl Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync + 'static,
    ) -> Self {
        self.publication_control = Some(Arc::new(control));
        self
    }

    pub fn with_route_control_expectation(mut self, expectation: C) -> Self {
        self.route_control_expectation = Some(expectation);
        self
    }

    /// Installs best-effort work that may run only after atomic publication.
    ///
    /// The callback cannot affect the committed generation and must suppress
    /// its own cache or observation failures.
    pub fn with_successful_publication(
        mut self,
        after_publication: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.after_successful_publication = Some(Arc::new(after_publication));
        self
    }

    pub fn scan(
        &self,
        sink: &mut SourceBackedGenerationSink<'_, L>,
    ) -> SourceBackedRouteResult<()> {
        (self.scan)(sink)
    }

    pub fn owns_source(&self, source: &SourceKey) -> SourceBackedRouteResult<bool> {
        (self.owns_source)(source)
    }

    pub fn revalidate(
        &self,
        target: SourceBackedRevalidationTarget<'_>,
    ) -> SourceBackedRouteResult<bool> {
        (self.revalidate)(target)
    }

    pub fn revalidate_complete_inventory(
        &self,
        inventory: &CertifiedSourceInventory,
    ) -> Option<SourceBackedRouteResult<bool>> {
        self.revalidate_complete_inventory
            .as_ref()
            .map(|revalidate| revalidate(inventory))
    }

    pub fn publication_revalidation(&self) -> Option<bool> {
        self.revalidate_at_publication
            .as_ref()
            .map(|revalidate| revalidate())
    }

    pub fn publication_control(&self) -> Option<SourceBackedRouteResult<Option<Vec<u8>>>> {
        self.publication_control.as_ref().map(|control| control())
    }

    pub fn watch_targets(&self) -> Option<SourceBackedRouteWatchTargets> {
        self.watch_targets.as_ref().and_then(|observe| observe())
    }

    pub fn route_control_expectation(&self) -> Option<&C> {
        self.route_control_expectation.as_ref()
    }

    pub fn uses_parallel_leaf_workers(&self) -> bool {
        self.uses_parallel_leaf_workers
    }

    pub fn after_successful_publication(&self) {
        if let Some(after_publication) = self.after_successful_publication.as_ref() {
            after_publication();
        }
    }

    pub fn scan_callback(&self) -> Arc<SourceBackedScanCallback<L>> {
        Arc::clone(&self.scan)
    }

    pub fn replace_scan_callback(&mut self, scan: Arc<SourceBackedScanCallback<L>>) {
        self.scan = scan;
    }

    pub fn revalidation_callback(&self) -> Arc<RevalidationCallback> {
        Arc::clone(&self.revalidate)
    }

    pub fn replace_revalidation_callback(&mut self, revalidate: Arc<RevalidationCallback>) {
        self.revalidate = revalidate;
    }

    pub fn replace_complete_inventory_revalidation_callback(
        &mut self,
        revalidate: Option<Arc<CompleteInventoryRevalidationCallback>>,
    ) {
        self.revalidate_complete_inventory = revalidate;
    }

    pub fn set_watch_targets(
        &mut self,
        observe: impl Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync + 'static,
    ) {
        self.watch_targets = Some(Arc::new(observe));
    }
}
