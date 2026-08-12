use super::super::*;
mod diagnostics;
mod refresh_control_plane;
pub use diagnostics::*;

#[cfg(test)]
thread_local! {
    static BASE_SOURCE_MANIFEST_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_base_source_manifest_visits() {
    BASE_SOURCE_MANIFEST_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(crate) fn base_source_manifest_visits() -> u64 {
    BASE_SOURCE_MANIFEST_VISITS.with(std::cell::Cell::get)
}

pub const MAX_RECORDED_SOURCE_BACKED_FAILURES: usize = 64;
pub const MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES: usize = 512;
pub const MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES: usize = 512;
pub const MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS: usize = 64;
pub const MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES: usize = 4 * 1024;
pub const MAX_SOURCE_BACKED_REJECTION_PAYLOAD_TYPE_BYTES: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in super::super) struct SourceBackedRecordProgressDelta {
    pub(in super::super) accepted_records: u64,
    pub(in super::super) completed_bytes: u64,
    pub(in super::super) session_ids: Vec<[u8; 32]>,
    pub(in super::super) messages: u64,
    pub(in super::super) tool_calls: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CoreRecordProgress {
    pub(crate) session_id: [u8; 32],
    pub(crate) messages: u64,
    pub(crate) tool_calls: u64,
}

impl CoreRecordProgress {
    pub(crate) fn from_record(record: &CoreRecord) -> Self {
        Self {
            session_id: record.session_id.digest(),
            messages: u64::from(record.event_type == "message"),
            tool_calls: u64::from(record.event_type == "tool_call"),
        }
    }
}

pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

// Bounded route, source, and record diagnostics live in the diagnostics module.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedDeletionDisposition {
    Deferred,
    Deleted,
}

/// Runtime metadata for one selected source route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata {
    pub source: ProviderSource,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
    pub route_identity: Option<SourceRouteIdentity>,
    pub watch_target_kind: SourceBackedWatchTargetKind,
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
    pub(crate) const fn is_logical_source_failure(self) -> bool {
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

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError {
    #[error(transparent)]
    Index(#[from] IndexError),
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
pub struct SourceBackedGenerationSink<'writer> {
    pub(in super::super) writer: &'writer mut GenerationWriter,
    pub(in super::super) core_record_preparer: CoreRecordPreparer,
    pub(in super::super) owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub(in super::super) complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub(in super::super) applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
    pub(in super::super) route_index: usize,
    pub(in super::super) route_identity: SourceRouteIdentity,
    pub(in super::super) base_route_control: Option<Vec<u8>>,
    pub(in super::super) resources: SourceBackedRouteResources,
    pub(in super::super) logical_source_failures: &'writer mut SourceBackedLogicalSourceFailures,
    pub(in super::super) record_rejections: &'writer mut SourceBackedRecordRejections,
    pub(in super::super) record_progress: Option<
        &'writer mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<()>,
    >,
    pub(in super::super) current_source_progress: Option<
        &'writer mut dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>,
    >,
    pub(in super::super) last_progress_session_id: Option<[u8; 32]>,
}

impl SourceBackedGenerationSink<'_> {
    pub fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.resources.reconciliation_demand()
    }

    pub(crate) fn base_route_control(&self) -> Option<&[u8]> {
        self.base_route_control.as_deref()
    }

    /// Carries unmentioned members of this exact route from the locked Core
    /// base while changed members are replaced atomically.
    pub(crate) fn retain_unstaged_base_route_sources(
        &mut self,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer
            .retain_unstaged_source_route_members(&self.route_identity)?;
        Ok(())
    }
}

#[derive(Clone)]
pub(in super::super) struct SourceOwner {
    pub(in super::super) route_index: usize,
    pub(in super::super) source: SourceKey,
    pub(in super::super) present: bool,
    pub(in super::super) revalidation: Option<SourceBackedRouteRevalidation>,
}

#[derive(Clone)]
pub(in super::super) enum SourceBackedRouteRevalidation {
    Source(CertifiedSource),
    Deletion(CertifiedSourceDeletion),
}

#[derive(Clone)]
pub(in super::super) struct CompleteInventoryOwner {
    pub(in super::super) route_index: usize,
    pub(in super::super) inventory: CertifiedSourceInventory,
}

impl SourceBackedGenerationSink<'_> {
    pub(crate) fn route_resources(&self) -> SourceBackedRouteResources {
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
        self.writer.base_manifest().and_then(|manifest| {
            manifest.sources.iter().find(|candidate| {
                #[cfg(test)]
                BASE_SOURCE_MANIFEST_VISITS.with(|visits| {
                    visits.set(visits.get().saturating_add(1));
                });
                candidate.observation().source().exact_descriptor_eq(source)
            })
        })
    }

    /// Returns only the prior certified sources retained by this route. A
    /// provider route must not infer ownership from the provider family alone:
    /// another retained route may intentionally cover the same input tree.
    pub(crate) fn base_route_sources(
        &self,
    ) -> SourceBackedCoordinatorResult<HashMap<SourceKey, CertifiedSource>> {
        let Some(manifest) = self.writer.base_manifest() else {
            return Ok(HashMap::new());
        };
        let Some(route) = manifest.source_route(&self.route_identity) else {
            return Ok(HashMap::new());
        };
        let mut sources = HashMap::with_capacity(route.sources().len());
        for source in route.sources() {
            let certificate = manifest
                .sources
                .iter()
                .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
                .cloned()
                .ok_or(IndexError::WriterInvariant(
                    "source-route snapshot names a missing certified source",
                ))?;
            sources.insert(source.clone(), certificate);
        }
        Ok(sources)
    }

    /// Whether an exact source is retained or has already been claimed by a
    /// different route in this refresh. Such a source is outside this route's
    /// mutation authority even when its selected filesystem root overlaps.
    pub(crate) fn source_owned_by_other_route(&self, source: &SourceKey) -> bool {
        let owned_in_attempt = self.owners.values().any(|owner| {
            owner.route_index != self.route_index && owner.source.exact_descriptor_eq(source)
        });
        owned_in_attempt
            || self.writer.base_manifest().is_some_and(|manifest| {
                manifest.source_routes().iter().any(|route| {
                    route.route_identity() != &self.route_identity
                        && route
                            .sources()
                            .iter()
                            .any(|candidate| candidate.exact_descriptor_eq(source))
                })
            })
    }

    pub fn begin_source(&mut self, source: SourceKey) -> SourceBackedCoordinatorResult<()> {
        self.claim_present(&source)?;
        self.writer.begin_source(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        self.claim_present(&source)?;
        Ok(self.writer.begin_source_append(source)?)
    }

    pub(crate) fn begin_source_append_from_base(
        &mut self,
        base: GenerationBaseCertifiedSource,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        let source = base.certificate().observation().source().clone();
        self.claim_present(&source)?;
        Ok(self.writer.begin_source_append_from_base(base)?)
    }

    pub fn add_core_record(&mut self, record: CoreRecord) -> SourceBackedCoordinatorResult<()> {
        let progress = CoreRecordProgress::from_record(&record);
        let emission = CoreRecordEmission::new(record, &self.resources, &self.core_record_preparer)
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

    fn accept_core_record_emission(
        &mut self,
        emission: CoreRecordEmission,
    ) -> SourceBackedCoordinatorResult<()> {
        let (prepared, reservation) = emission.into_prepared();
        self.writer.add_prepared_core_record(prepared)?;
        drop(reservation);
        Ok(())
    }

    pub(crate) fn add_core_records_with_completed_bytes(
        &mut self,
        records: Vec<CoreRecord>,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<()> {
        let accepted_records = u64::try_from(records.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record page count overflowed",
            ))
        })?;
        let mut progress = CoreRecordBatchProgress::default();
        for record in records {
            progress.push(CoreRecordProgress::from_record(&record));
            let emission =
                CoreRecordEmission::new(record, &self.resources, &self.core_record_preparer)
                    .map_err(SourceBackedCoordinatorError::CoreEmission)?;
            self.accept_core_record_emission(emission)?;
        }
        self.report_record_progress(
            accepted_records,
            completed_bytes,
            &progress.session_ids,
            progress.messages,
            progress.tool_calls,
        )
    }

    pub(crate) fn add_core_record_emission_batch(
        &mut self,
        batch: CoreRecordEmissionBatch,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<()> {
        let accepted_records = u64::try_from(batch.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record emission batch count overflowed",
            ))
        })?;
        let progress = batch.progress().clone();
        let (prepared_records, reservation) = batch.into_prepared();
        for prepared in prepared_records {
            self.writer.add_prepared_core_record(prepared)?;
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

    pub(crate) fn record_logical_source_failure(
        &mut self,
        source: SourceKey,
        failure: SourceBackedRouteError,
        carried_forward: bool,
    ) -> SourceBackedCoordinatorResult<()> {
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

    pub(crate) fn record_rejection(&mut self, rejection: SourceBackedRecordRejectionDraft) {
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

    pub(crate) fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        let (rejections, omitted) = rejections.into_parts();
        for rejection in rejections {
            self.record_rejection(rejection);
        }
        self.record_omitted_rejections(omitted);
    }

    pub(crate) fn record_omitted_rejections(&mut self, omitted: usize) {
        self.record_rejections.record_omitted(omitted);
    }

    pub fn report_completed_bytes(&mut self, bytes: u64) -> SourceBackedCoordinatorResult<()> {
        self.report_record_progress(0, bytes, &[], 0, 0)
    }

    fn report_record_progress(
        &mut self,
        accepted_records: u64,
        completed_bytes: u64,
        session_ids: &[[u8; 32]],
        messages: u64,
        tool_calls: u64,
    ) -> SourceBackedCoordinatorResult<()> {
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
    ) -> SourceBackedCoordinatorResult<()> {
        let source = certificate.observation().source().clone();
        self.writer.certify_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<()> {
        let certificate = append.current().clone();
        let source = certificate.observation().source().clone();
        self.writer.certify_source_append(append)?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn retain_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim_present(certificate.observation().source())?;
        let source = certificate.observation().source().clone();
        self.writer.retain_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_complete_inventory(inventory.clone())?;
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
    ) -> SourceBackedCoordinatorResult<SourceBackedDeletionDisposition> {
        if !deletion.verifies(&inventory) {
            return Err(SourceBackedCoordinatorError::InvalidDeletionWitness);
        }
        self.claim_absent(deletion.source())?;
        self.writer
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
    ) -> SourceBackedCoordinatorResult<()> {
        self.begin_source(certificate.observation().source().clone())?;
        for record in core_records {
            self.add_core_record(record)?;
        }
        self.certify_source(certificate)
    }

    pub(in super::super) fn claim_present(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(source, true)
    }

    pub(in super::super) fn claim_absent(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(source, false)
    }

    fn claim(&mut self, source: &SourceKey, present: bool) -> SourceBackedCoordinatorResult<()> {
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
    ) -> SourceBackedCoordinatorResult<()> {
        let owner = self
            .owners
            .get_mut(&source.identity().digest())
            .filter(|owner| {
                owner.route_index == self.route_index
                    && owner.source.exact_descriptor_eq(source)
                    && owner.revalidation.is_none()
            })
            .ok_or(IndexError::WriterInvariant(
                "source certification lost its route-local owner",
            ))?;
        owner.revalidation = Some(revalidation);
        Ok(())
    }
}

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

type ScanCallback = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
pub(super) type SourcePredicate = dyn Fn(&SourceKey) -> SourceBackedRouteResult<bool> + Send + Sync;
type RevalidationCallback = dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> SourceBackedRouteResult<bool>
    + Send
    + Sync;
type CompleteInventoryRevalidationCallback =
    dyn Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool> + Send + Sync;
type SuccessfulPublicationCallback = dyn Fn() + Send + Sync;
type RoutePublicationRevalidationCallback = dyn Fn() -> bool + Send + Sync;
type RoutePublicationControlCallback =
    dyn Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync;
type WatchTargetsCallback = dyn Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync;

#[derive(Debug, Clone, Default)]
pub(in super::super) struct SourceBackedRouteWatchTargets {
    pub(in super::super) sqlite_databases: BTreeSet<PathBuf>,
    pub(in super::super) authority_paths: BTreeSet<PathBuf>,
}

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
#[derive(Clone)]
pub struct SourceBackedRouteDriver {
    pub(in super::super) scan: Arc<ScanCallback>,
    pub(in super::super) owns_source: Arc<SourcePredicate>,
    pub(in super::super) revalidate: Arc<RevalidationCallback>,
    pub(in super::super) revalidate_complete_inventory:
        Option<Arc<CompleteInventoryRevalidationCallback>>,
    pub(in super::super) after_successful_publication: Option<Arc<SuccessfulPublicationCallback>>,
    pub(in super::super) revalidate_at_publication:
        Option<Arc<RoutePublicationRevalidationCallback>>,
    pub(in super::super) publication_control: Option<Arc<RoutePublicationControlCallback>>,
    pub(in super::super) watch_targets: Option<Arc<WatchTargetsCallback>>,
    pub(in super::super) route_control_expectation: Option<SourceBackedRouteControlExpectation>,
    pub(in super::super) uses_parallel_leaf_workers: bool,
}

impl fmt::Debug for SourceBackedRouteDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl SourceBackedRouteDriver {
    pub fn new(
        scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
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

    pub(crate) fn new_fallible(
        scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
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

    pub(crate) fn with_parallel_leaf_workers(mut self) -> Self {
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

    pub(crate) fn with_fallible_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.revalidate_complete_inventory = Some(Arc::new(revalidate));
        self
    }

    pub(crate) fn with_publication_revalidation(
        mut self,
        revalidate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_at_publication = Some(Arc::new(revalidate));
        self
    }

    pub(crate) fn with_publication_control(
        mut self,
        control: impl Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync + 'static,
    ) -> Self {
        self.publication_control = Some(Arc::new(control));
        self
    }

    pub(crate) fn with_route_control_expectation(
        mut self,
        expectation: SourceBackedRouteControlExpectation,
    ) -> Self {
        self.route_control_expectation = Some(expectation);
        self
    }

    /// Installs best-effort work that may run only after atomic publication.
    ///
    /// The callback cannot affect the committed generation and must suppress
    /// its own cache or observation failures.
    pub(crate) fn with_successful_publication(
        mut self,
        after_publication: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.after_successful_publication = Some(Arc::new(after_publication));
        self
    }
}

#[derive(Debug, Clone)]
pub(in super::super) struct HermesRouteRetirement {
    pub(in super::super) route_identity: SourceRouteIdentity,
    pub(in super::super) database_identity: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    pub(in super::super) metadata: SourceBackedRouteMetadata,
    pub(in super::super) driver: Option<SourceBackedRouteDriver>,
    pub(in super::super) certified_missing_paths: Vec<PathBuf>,
    pub(in super::super) retire_after_success: Vec<SourceRouteIdentity>,
    pub(in super::super) hermes_retire_after_success: Vec<HermesRouteRetirement>,
    pub(in super::super) codex_generation_participant: Option<usize>,
}

impl SourceBackedRoute {
    #[cfg(test)]
    pub(in crate::provider) fn explicit_manual_unchecked_for_test(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        certified_source_format: &'static str,
        watch_target_kind: SourceBackedWatchTargetKind,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let route_identity = source_backed_route_identity(
            &source,
            certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn certified_missing(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        let path = source.path.clone();
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: None,
            certified_missing_paths: vec![path],
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
                route_identity: None,
                watch_target_kind: SourceBackedWatchTargetKind::Path,
            },
            driver: None,
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            hermes_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    pub(in super::super) routes: Vec<SourceBackedRoute>,
    pub(in super::super) codex_generation: Option<Arc<CodexGenerationNormalizationCoordinatorV0>>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        if let Some(identity) = route.metadata.route_identity.as_ref() {
            if let Some(existing) = self
                .routes
                .iter_mut()
                .find(|existing| existing.metadata.route_identity.as_ref() == Some(identity))
            {
                if existing.driver.is_some() {
                    return;
                }
                if route.driver.is_some() {
                    *existing = route;
                    return;
                }
                existing
                    .certified_missing_paths
                    .extend(route.certified_missing_paths);
                existing.certified_missing_paths.sort();
                existing.certified_missing_paths.dedup();
                return;
            }
        }
        self.routes.push(route);
    }

    /// Binds exact carried base routes to an executable replacement route.
    /// Retirement is applied only after that replacement scans and terminally
    /// revalidates successfully; failed replacements retain the base routes.
    pub fn retire_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.driver.is_none() {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route.retire_after_success.extend(retired);
        route.retire_after_success.sort();
        route.retire_after_success.dedup();
        if route
            .retire_after_success
            .binary_search(replacement)
            .is_ok()
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Registers stale automatic Hermes routes as conditional retirement
    /// candidates. A candidate is authorized only when the replacement's
    /// successful control reports the same stable physical database identity.
    pub fn retire_hermes_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = (SourceRouteIdentity, [u8; 32])>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.driver.is_none()
            || route.metadata.source.provider != CaptureProvider::Hermes
            || route.metadata.selection != Some(SourceBackedRouteSelection::Automatic)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route
            .hermes_retire_after_success
            .extend(
                retired
                    .into_iter()
                    .map(
                        |(route_identity, database_identity)| HermesRouteRetirement {
                            route_identity,
                            database_identity,
                        },
                    ),
            );
        route.hermes_retire_after_success.sort_by(|left, right| {
            left.route_identity
                .cmp(&right.route_identity)
                .then(left.database_identity.cmp(&right.database_identity))
        });
        route.hermes_retire_after_success.dedup_by(|left, right| {
            left.route_identity == right.route_identity
                && left.database_identity == right.database_identity
        });
        if route
            .hermes_retire_after_success
            .iter()
            .any(|candidate| &candidate.route_identity == replacement)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.iter().map(SourceBackedRoute::metadata)
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .count()
    }

    /// Returns whether any executable route selected by this exact refresh can
    /// consume the source-scanner half of the coordinated CPU budget.
    pub fn selected_routes_use_parallel_leaf_workers(
        &self,
        scope: &SourceBackedRefreshScope,
    ) -> bool {
        self.routes.iter().any(|route| {
            route
                .driver
                .as_ref()
                .is_some_and(|driver| driver.uses_parallel_leaf_workers)
                && match scope {
                    SourceBackedRefreshScope::All => true,
                    SourceBackedRefreshScope::Exact(selected) => route
                        .metadata
                        .route_identity
                        .as_ref()
                        .is_some_and(|identity| selected.contains(identity)),
                }
        })
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_none())
            .filter(|route| route.certified_missing_paths.is_empty())
            .count()
    }
}

/// Derives the canonical identity for a source's landed automatic route.
///
/// This intentionally accepts sources that failed registration so callers can
/// match route-local failures to a retained healthy route from the same source.
pub fn automatic_source_backed_route_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let known = landed_format_route(source.provider, source.source_format)
        .filter(|route| route.automatic)
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                format!(
                    "source format {:?} has no landed automatic route",
                    source.source_format
                ),
            )
        })?;
    source_backed_route_identity(
        source,
        known.certified_source_format,
        SourceBackedRouteSelection::Automatic,
        known.selector_authority,
    )
}

/// Derives the stable source-scoped failure identity used by refresh receipts
/// and direct unsupported-source diagnostics.
pub fn source_backed_source_failure_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<String> {
    let certified_source_format = landed_format_route(source.provider, source.source_format)
        .map_or(source.source_format, |route| route.certified_source_format);
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-failure-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    let path = source.path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    Ok(format!("{:x}", digest.finalize()))
}

fn source_backed_route_identity(
    source: &ProviderSource,
    certified_source_format: &str,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(match selection {
        SourceBackedRouteSelection::Automatic => b"automatic".as_slice(),
        SourceBackedRouteSelection::ExplicitManual => b"explicit".as_slice(),
    });
    digest.update([0]);
    digest.update(match selector_authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => b"discovered-winner".as_slice(),
        SourceBackedSelectorAuthority::ExplicitPath => b"explicit-path".as_slice(),
        SourceBackedSelectorAuthority::CatalogLineage => b"catalog-lineage".as_slice(),
        SourceBackedSelectorAuthority::ExactCwd => b"exact-cwd".as_slice(),
        SourceBackedSelectorAuthority::NamedSurface => b"named-surface".as_slice(),
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            b"selected-with-retained-explicit".as_slice()
        }
    });
    // Discovered-winner routes deliberately keep path-independent identity so
    // moving the selected provider root remains an in-place replacement.
    // Catalog-lineage routes instead represent independently owned catalogs;
    // automatic NanoClaw discovery may therefore register several checkouts.
    if selection == SourceBackedRouteSelection::ExplicitManual
        || selector_authority == SourceBackedSelectorAuthority::CatalogLineage
    {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
    } else if source.provider == CaptureProvider::Hermes {
        let profile =
            crate::provider::providers::hermes::source_backed::hermes_automatic_profile_name(
                &source.path,
            )
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
        if profile != "default" {
            // Hermes discovery intentionally multiplexes independently owned
            // named profiles. Keep the historical default route identity, but
            // give every validated named profile a stable path-independent
            // logical slot so registry de-duplication cannot collapse them.
            digest.update(b"\0hermes-profile\0");
            digest.update((profile.len() as u64).to_be_bytes());
            digest.update(profile.as_bytes());
        }
    }
    SourceRouteIdentity::from_sha256(format!("{:x}", digest.finalize())).map_err(Into::into)
}
