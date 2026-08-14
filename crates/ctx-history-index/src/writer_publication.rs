use super::*;
use crate::merge_policy::deletion_density_exceeds_limit;
use ctx_history_index_format::{
    open_publication_candidate, verify_and_bind_publication_candidate_with_progress,
    verify_and_bind_reusable_publication, CandidatePublicationVerificationError,
    ReusablePublicationError, VerifiedCandidatePublication,
};
use std::collections::BTreeMap;

#[cfg(test)]
thread_local! {
    static BASE_MANIFEST_SOURCE_MATERIALIZATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_manifest_materialization_visits() {
    BASE_MANIFEST_SOURCE_MATERIALIZATIONS.with(|visits| visits.set(0));
    PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(crate) fn manifest_materialization_visits() -> (u64, u64) {
    (
        BASE_MANIFEST_SOURCE_MATERIALIZATIONS.with(std::cell::Cell::get),
        PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS.with(std::cell::Cell::get),
    )
}

struct CommitGenerationOutcome {
    receipt: CommitReceipt,
    disposition: PublicationDisposition,
    verified_index: Option<VerifiedIndex>,
}

impl CommitGenerationOutcome {
    fn into_receipt(self) -> CommitReceipt {
        self.receipt
    }

    fn into_published_generation(self) -> Result<PublishedGeneration> {
        let verified_index = self.verified_index.ok_or(IndexError::WriterInvariant(
            "metadata publication completed without its verified index",
        ))?;
        PublishedGeneration::new(self.receipt, self.disposition, verified_index)
    }
}

struct VerifiedCandidate {
    slot: GenerationSlot,
    publication: VerifiedCandidatePublication,
}

impl GenerationWriter {
    /// Rebinds opaque owner metadata to an exact reused generation without
    /// changing its logical or physical index payload.
    pub fn republish_current_publication_metadata(
        self,
        expected_generation_id: &str,
        publication_metadata: Vec<u8>,
    ) -> Result<VerifiedIndex> {
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        let pointer = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "publication metadata republish requires an active generation",
            ))?;
        let generation_id = self
            .base_manifest()
            .ok_or(IndexError::WriterInvariant(
                "publication metadata republish requires a base manifest",
            ))?
            .generation_id()?;
        if generation_id != expected_generation_id
            || pointer.active().generation_id() != expected_generation_id
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if publication_metadata.len() > MAX_PUBLICATION_METADATA_BYTES {
            return Err(IndexError::PublicationMetadataTooLarge {
                actual: publication_metadata.len(),
                maximum: MAX_PUBLICATION_METADATA_BYTES,
            });
        }
        let outcome = republish_current_with_publication_metadata(
            &self.root,
            pointer,
            &self.writer_options,
            publication_metadata.into(),
        )?;
        let published_pointer = match outcome {
            CurrentRepublishOutcome::Published(pointer) => pointer,
            CurrentRepublishOutcome::CommittedVisible { recovery, .. }
            | CurrentRepublishOutcome::CommittedRecoveryRequired { recovery } => {
                return Err(IndexError::CommittedGenerationNeedsRecovery {
                    generation_id: recovery.generation_id().to_owned(),
                    stage: "publication metadata republish",
                    detail: recovery.detail().to_owned(),
                });
            }
        };
        best_effort_post_republish_cleanup(&self.root, &published_pointer);
        let verified = VerifiedIndex::open_pinned(&self.root)?;
        if verified.generation_id() != expected_generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        Ok(verified)
    }

    pub(super) fn writer_mut(&mut self) -> Result<&mut IndexWriter<IndexDocument>> {
        if self.writer.is_none() {
            #[cfg(test)]
            if let Some(hook) = self.before_writer_handoff.take() {
                hook();
            }

            if self.candidate_directory_name.is_none() {
                let candidate = create_candidate_generation(
                    &self.root,
                    self.active_pointer
                        .as_ref()
                        .map(ActiveGenerationPointer::active),
                )?;
                self.index = candidate.index;
                self.fields = fields_from_schema(&self.index.schema())?;
                validate_schema(&self.index.schema())?;
                self.candidate_directory_name = Some(candidate.directory_name);
            }

            let writer = construct_index_writer_with_retry(&self.index, &self.writer_options)?;
            #[cfg(test)]
            self.index_writer_constructions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current_metas = self.index.load_metas()?;
            let expected_generation = self
                .base_manifest()
                .map(GenerationManifest::generation_id)
                .transpose()?;
            let current_generation = payload_generation_id(&current_metas)?;
            let expected_segments = self
                .base_publication
                .as_ref()
                .map(PinnedPublication::searcher)
                .map(searcher_generation)
                .unwrap_or_default();
            if current_metas.opstamp != self.base_opstamp
                || current_generation != expected_generation
                || meta_generation(&current_metas) != expected_segments
            {
                return Err(IndexError::ConcurrentGenerationChange);
            }

            writer.set_merge_policy(Box::new(LexicalMergePolicy::default()));
            let _ = writer.garbage_collect_files().wait()?;
            self.writer = Some(writer);
        }
        self.writer.as_mut().ok_or(IndexError::WriterInvariant(
            "lazy writer construction completed without a writer",
        ))
    }

    /// Prevents segment merging in tests without exposing the writer or its
    /// document type.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_disable_merges(&mut self) -> Result<()> {
        self.writer_mut()?
            .set_merge_policy(Box::<tantivy::indexer::NoMergePolicy>::default());
        Ok(())
    }

    /// Publishes one atomic lexical generation.
    ///
    /// `revalidate` runs after Tantivy has flushed all staged indexing workers
    /// and immediately before the immutable manifest and candidate commit.
    pub fn commit<F>(self, revalidate: F) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
    {
        Ok(self
            .commit_generation(revalidate, |_| false, |_| Ok(None), false, |_| Ok(()))?
            .into_receipt())
    }

    /// Publishes with refresh-owned opaque metadata constructed from the final
    /// terminally revalidated logical generation.
    ///
    /// Exact no-op/reuse does not invoke `metadata_factory`; callers must use
    /// [`PublicationDisposition`] to distinguish old generation metadata from
    /// bytes constructed for the current request.
    pub fn commit_with_publication_metadata<F, M>(
        self,
        revalidate: F,
        metadata_factory: M,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
    {
        self.commit_generation(
            revalidate,
            |_| false,
            |context| metadata_factory(context).map(Some),
            true,
            |_| Ok(()),
        )?
        .into_published_generation()
    }

    /// Publishes one atomic lexical generation with terminal revalidation for
    /// each current complete-inventory certificate registered on the writer.
    pub fn commit_with_complete_inventory_revalidation<F, I>(
        self,
        revalidate: F,
        revalidate_inventory: I,
    ) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self
            .commit_generation(
                revalidate,
                revalidate_inventory,
                |_| Ok(None),
                false,
                |_| Ok(()),
            )?
            .into_receipt())
    }

    /// Publishes with terminal source/inventory revalidation and a final
    /// refresh-owned opaque metadata factory.
    pub fn commit_with_complete_inventory_revalidation_and_publication_metadata<F, I, M>(
        self,
        revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
    {
        self.commit_generation(
            revalidate,
            revalidate_inventory,
            |context| metadata_factory(context).map(Some),
            true,
            |_| Ok(()),
        )?
        .into_published_generation()
    }

    /// Publishes with terminal revalidation, owner metadata, and real
    /// whole-run publication stage transitions.
    pub fn commit_with_complete_inventory_revalidation_and_publication_metadata_and_progress<
        F,
        I,
        M,
        P,
    >(
        self,
        revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
        report_progress: P,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
        P: FnMut(PublicationStage) -> Result<()>,
    {
        self.commit_generation(
            revalidate,
            revalidate_inventory,
            |context| metadata_factory(context).map(Some),
            true,
            report_progress,
        )?
        .into_published_generation()
    }

    fn commit_generation<F, I, M, P>(
        mut self,
        mut revalidate: F,
        mut revalidate_inventory: I,
        metadata_factory: M,
        return_verified_index: bool,
        mut report_progress: P,
    ) -> Result<CommitGenerationOutcome>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Option<Vec<u8>>>,
        P: FnMut(PublicationStage) -> Result<()>,
    {
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        self.validate_source_route_plan_complete()?;
        if let Some(witness) = self.exact_replay_inventory_witness()? {
            for route in witness.base.source_routes().iter().filter(|route| {
                !self
                    .source_route_plan
                    .as_ref()
                    .is_some_and(|plan| plan.carried_from_base.contains(route.route_identity()))
                    && !self
                        .partially_reconciled_routes
                        .contains(route.route_identity())
            }) {
                for source in route.sources() {
                    let certificate = witness
                        .base
                        .sources
                        .binary_search_by_key(&source.identity().digest(), |candidate| {
                            candidate.observation().source().identity().digest()
                        })
                        .ok()
                        .and_then(|index| witness.base.sources.get(index))
                        .ok_or(IndexError::WriterInvariant(
                            "validated route member is missing its source certificate",
                        ))?;
                    if !revalidate(RevalidationTarget::Source(certificate)) {
                        return Err(IndexError::SourceInvalidated(
                            certificate.observation().source().identity().to_string(),
                        ));
                    }
                }
            }
            for inventory in &self.complete_inventories {
                if !revalidate_inventory(inventory) {
                    return Err(IndexError::CompleteInventoryInvalidated {
                        provider: inventory.observation().provider().to_owned(),
                        authority_namespace: inventory
                            .observation()
                            .authority_namespace()
                            .to_owned(),
                    });
                }
            }
            for (route, revalidate_route) in &self.route_publication_revalidations {
                if !revalidate_route() {
                    return Err(IndexError::SourceInvalidated(route.as_str().to_owned()));
                }
            }
            let opstamp = self.base_opstamp;
            report_progress(PublicationStage::PhysicalVerification)?;
            let reused = self.reused_generation(opstamp, return_verified_index)?;
            report_progress(PublicationStage::Activation)?;
            return Ok(reused);
        }

        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }

        let manifest = self.next_manifest()?;
        if finish_identical_staging(
            &mut self,
            &manifest,
            &mut revalidate,
            &mut revalidate_inventory,
        )? {
            self.discard_candidate()?;
            let opstamp = self.base_opstamp;
            report_progress(PublicationStage::PhysicalVerification)?;
            let reused = self.reused_generation(opstamp, return_verified_index)?;
            report_progress(PublicationStage::Activation)?;
            return Ok(reused);
        }

        // Build opaque owner metadata from the complete staged manifest before
        // the terminal source fence. The bytes are bound only if every source
        // and inventory revalidation below succeeds, so observations sampled
        // by the owner cannot describe state newer than the Core projection
        // that the fence accepts.
        let generation_id = manifest.generation_id()?;
        let publication_metadata =
            metadata_factory(PublicationMetadataContext::new(&generation_id, &manifest))?;

        self.writer_mut()?;
        let candidate_path = self.candidate_path()?;
        let previous_generation_id = self
            .base_manifest()
            .map(GenerationManifest::generation_id)
            .transpose()?;
        let root = self.root.clone();
        let mut prepared = self
            .writer
            .as_mut()
            .ok_or(IndexError::WriterInvariant(
                "mutating commit is missing its lazy writer",
            ))?
            .prepare_commit()?;
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            if !revalidate(RevalidationTarget::Source(certificate)) {
                let source = pending.source.identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }
        for removal in self.deletions.values() {
            if !revalidate(RevalidationTarget::Deletion(&removal.proof)) {
                let source = removal.source().identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }
        for (route, revalidate_route) in &self.route_publication_revalidations {
            if !revalidate_route() {
                let route = route.as_str().to_owned();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(route));
            }
        }
        for inventory in &self.complete_inventories {
            if !revalidate_inventory(inventory) {
                let error = IndexError::CompleteInventoryInvalidated {
                    provider: inventory.observation().provider().to_owned(),
                    authority_namespace: inventory.observation().authority_namespace().to_owned(),
                };
                prepared.abort()?;
                return Err(error);
            }
        }

        let payload =
            match canonical_commit_payload(&generation_id, publication_metadata.as_deref()) {
                Ok(payload) => payload,
                Err(error) => {
                    prepared.abort()?;
                    return Err(error);
                }
            };
        if let Err(error) = write_manifest(&root, &generation_id, &manifest) {
            let _ = prepared.abort();
            return Err(error);
        }
        prepared.set_payload(&payload);
        #[cfg(test)]
        if let Some(hook) = self.before_candidate_commit.take() {
            hook(&candidate_path);
        }
        report_progress(PublicationStage::Merging)?;
        let commit_result = prepared.commit();
        #[cfg(test)]
        let commit_result = if self.return_commit_error_after_visibility {
            commit_result.and_then(|_| {
                Err(tantivy::TantivyError::InvalidArgument(
                    "injected error after the candidate commit became visible".to_owned(),
                ))
            })
        } else {
            commit_result
        };
        drop(payload);
        drop(publication_metadata);
        let writer = self.writer.take().ok_or(IndexError::WriterInvariant(
            "candidate commit is missing its lazy writer",
        ))?;
        writer.wait_merging_threads()?;
        let (opstamp, reconciled_commit_error) = match commit_result {
            Ok(opstamp) => (opstamp, None),
            Err(error) => {
                let commit_error = error.to_string();
                let opstamp = reconcile_commit_error(
                    &self.index,
                    &generation_id,
                    previous_generation_id.as_deref(),
                    error,
                )?;
                (opstamp, Some(commit_error))
            }
        };
        // Merge completion fixes the exact writer-produced segment and delete
        // topology. Verification may rely on canonical staging only while this
        // ephemeral fence still matches the bytes it is about to publish.
        let committed_candidate_generation = meta_generation(&self.index.load_metas()?);

        #[cfg(test)]
        if let Some(hook) = self.after_candidate_commit.take() {
            hook(&candidate_path);
        }
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_switch.take() {
            hook(&candidate_path);
        }
        report_progress(PublicationStage::Syncing)?;
        sync_generation(&candidate_path)?;

        let directory_name =
            self.candidate_directory_name
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "verified candidate has no generation directory",
                ))?;
        report_progress(PublicationStage::PhysicalVerification)?;
        let verified = self
            .verify_candidate(
                &candidate_path,
                &generation_id,
                &directory_name,
                &committed_candidate_generation,
                || report_progress(PublicationStage::LogicalVerification),
            )
            .map_err(
                |verification_error| match reconciled_commit_error.as_ref() {
                    None => verification_error,
                    Some(commit_error) => IndexError::CommittedGenerationNeedsRecovery {
                        generation_id: generation_id.clone(),
                        stage: "candidate commit reconciliation",
                        detail: format!(
                            "{commit_error}; candidate commit completed but verification failed: \
                             {verification_error}"
                        ),
                    },
                },
            )?;
        drop(manifest);
        let next_pointer = ActiveGenerationPointer::new(
            verified.slot.clone(),
            self.base_publication.as_ref().and_then(|_| {
                self.active_pointer
                    .as_ref()
                    .map(|pointer| pointer.active().clone())
            }),
        )?;
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_publication.take() {
            hook(&candidate_path);
        }
        report_progress(PublicationStage::Activation)?;
        match publish_active_generation_pointer(&root, &next_pointer) {
            Ok(PointerPublicationOutcome::Durable) => {}
            Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
                return Err(IndexError::CommittedGenerationNeedsRecovery {
                    generation_id,
                    stage: "active generation pointer durability",
                    detail,
                });
            }
            Err(error) => {
                return Err(self.classify_pointer_failure(&generation_id, &next_pointer, error));
            }
        }
        #[cfg(test)]
        if let Some(hook) = self.after_pointer_switch.take() {
            hook(&candidate_path);
        }
        // The durable pointer is authoritative now. Writer open retries every
        // cleanup below, so treat each attempt independently and never turn a
        // published generation into a failed refresh because reclamation was
        // temporarily obstructed. A malformed lease suppresses every reclaim:
        // treating it as absent could delete the one target it was meant to
        // preserve before the next strict writer open reports it.
        let _ = clear_active_generation_rebuild_marker(&root);
        if let Ok(retention_lease) = load_generation_retention_lease(&root) {
            let mut retained_generation_ids = std::iter::once(next_pointer.active())
                .chain(next_pointer.previous())
                .map(|slot| slot.generation_id().to_owned())
                .collect::<Vec<_>>();
            retained_generation_ids.extend(
                retention_lease
                    .as_ref()
                    .map(|lease| lease.generation_id().to_owned()),
            );
            let _ = reclaim_inactive_generation_directories(
                &root,
                Some(&next_pointer),
                retention_lease.as_ref(),
            );
            let _ = reclaim_unreferenced_manifests(&root, &retained_generation_ids);
            let _ = reclaim_unreferenced_certifications(
                &root,
                Some(&next_pointer),
                retention_lease.as_ref(),
            );
        }
        let _ = publication::certify_activated_generation(
            &root,
            &next_pointer,
            next_pointer.active(),
            verified.publication.publication().searcher().index(),
            verified.publication.physical_integrity_audit(),
        );

        let receipt = CommitReceipt::from_verified_manifest(
            opstamp,
            generation_id.clone(),
            std::sync::Arc::clone(verified.publication.publication().shared_manifest()),
        );
        let verified_index = return_verified_index.then(|| {
            VerifiedIndex::from_verified_publication(verified.publication.into_publication())
        });
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Published,
            verified_index,
        })
    }

    fn verify_candidate<P>(
        &self,
        candidate_path: &Path,
        generation_id: &str,
        directory_name: &str,
        committed_candidate_generation: &BTreeMap<String, Option<u64>>,
        report_logical_verification: P,
    ) -> Result<VerifiedCandidate>
    where
        P: FnOnce() -> Result<()>,
    {
        let candidate = open_publication_candidate(&self.root, candidate_path)?;
        if candidate.generation_id() != generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if &meta_generation(candidate.metas()) != committed_candidate_generation {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        for segment in &candidate.metas().segments {
            if deletion_density_exceeds_limit(segment) {
                return Err(IndexError::CandidateDeletionDensityExceeded {
                    deleted_documents: u64::from(segment.num_deleted_docs()),
                    max_documents: u64::from(segment.max_doc()),
                });
            }
        }
        let publication = verify_and_bind_publication_candidate_with_progress(
            candidate,
            self.active_pointer.as_ref(),
            self.base_publication.as_ref(),
            self.active_pointer
                .as_ref()
                .map(|pointer| (&*self.root, pointer, pointer.active())),
            report_logical_verification,
        )
        .map_err(|error| match error {
            CandidatePublicationVerificationError::Candidate(error) => error,
            CandidatePublicationVerificationError::Reusable(ReusablePublicationError::Binding(
                error,
            )) => error,
            CandidatePublicationVerificationError::Reusable(
                ReusablePublicationError::Integrity(error),
            ) => {
                let active = self
                    .active_pointer
                    .as_ref()
                    .expect("reusable publication verification has active authority")
                    .active();
                classify_active_integrity_failure(&self.root, active, error)
            }
        })?;
        let slot = GenerationSlot::new(
            generation_id.to_owned(),
            directory_name.to_owned(),
            publication.physical_integrity_audit().digest().to_owned(),
        )?;
        Ok(VerifiedCandidate { slot, publication })
    }

    fn reused_generation(
        mut self,
        opstamp: u64,
        return_verified_index: bool,
    ) -> Result<CommitGenerationOutcome> {
        let base = self
            .base_publication
            .take()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its base publication",
            ))?;
        let pointer = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its active pointer",
            ))?;
        let active = pointer.active();
        if active.generation_id() != base.generation_id() {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let publication = verify_and_bind_reusable_publication(&self.root, pointer, active, base)
            .map_err(|error| match error {
            ReusablePublicationError::Binding(error) => error,
            ReusablePublicationError::Integrity(error) => {
                self.classify_reusable_integrity_failure(active, error)
            }
        })?;
        let receipt = CommitReceipt::from_verified_manifest(
            opstamp,
            publication.generation_id().to_owned(),
            std::sync::Arc::clone(publication.shared_manifest()),
        );
        let verified_index =
            return_verified_index.then(|| VerifiedIndex::from_verified_publication(publication));
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Reused,
            verified_index,
        })
    }

    fn classify_reusable_integrity_failure(
        &self,
        active: &GenerationSlot,
        error: IndexError,
    ) -> IndexError {
        classify_active_integrity_failure(&self.root, active, error)
    }

    fn classify_pointer_failure(
        &self,
        generation_id: &str,
        expected: &ActiveGenerationPointer,
        error: IndexError,
    ) -> IndexError {
        match load_active_generation_pointer(&self.root) {
            Ok(Some(pointer)) if &pointer == expected => {
                IndexError::CommittedGenerationNeedsRecovery {
                    generation_id: generation_id.to_owned(),
                    stage: "active generation pointer durability",
                    detail: error.to_string(),
                }
            }
            Ok(pointer) if pointer == self.active_pointer => error,
            Ok(pointer) => IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.to_owned(),
                stage: "active generation pointer reconciliation",
                detail: format!("{error}; active pointer is {pointer:?}"),
            },
            Err(reconcile_error) => IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.to_owned(),
                stage: "active generation pointer reconciliation",
                detail: format!("{error}; pointer reload failed: {reconcile_error}"),
            },
        }
    }

    fn candidate_path(&self) -> Result<PathBuf> {
        let directory =
            self.candidate_directory_name
                .as_deref()
                .ok_or(IndexError::WriterInvariant(
                    "candidate generation directory is missing",
                ))?;
        Ok(self.root.join(INDEX_GENERATIONS_DIRECTORY).join(directory))
    }

    fn discard_candidate(&mut self) -> Result<()> {
        let Some(directory) = self.candidate_directory_name.take() else {
            return Ok(());
        };
        fs::remove_dir_all(self.root.join(INDEX_GENERATIONS_DIRECTORY).join(directory))?;
        sync_directory(&self.root.join(INDEX_GENERATIONS_DIRECTORY))?;
        Ok(())
    }

    fn next_manifest(&self) -> Result<GenerationManifest> {
        self.validate_source_route_plan_complete()?;
        let deleted_sources = self
            .deletions
            .keys()
            .chain(&self.route_deletions)
            .map(|source| source.identity().digest())
            .collect::<BTreeSet<_>>();
        let mut source_upserts = BTreeMap::<[u8; 32], CertifiedSource>::new();
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            source_upserts.insert(pending.source.identity().digest(), certificate.clone());
        }
        let sources = merge_manifest_sources(
            self.base_manifest().map_or(&[][..], |base| &base.sources),
            source_upserts,
            &deleted_sources,
        );
        let record_aggregates = staging::manifest_record_aggregates(self, &sources)?;
        let mut source_routes = if let Some(routes) = &self.present_source_routes {
            routes.clone()
        } else {
            implicit_source_routes(&sources)?
        };
        for route in &mut source_routes {
            let Some(delta) = self.partial_source_route_deltas.get(route.route_identity()) else {
                continue;
            };
            if route.missing_state().is_some() {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "partial route {} cannot carry missing state",
                    route.route_identity().as_str()
                )));
            }
            *route = SourceRouteSnapshot::present(
                route.route_identity().clone(),
                merge_partial_route_members(route.sources(), delta),
            )?;
        }
        source_routes.extend(self.observed_missing_routes.values().cloned());
        GenerationManifest::from_parts_with_record_aggregates(
            sources,
            record_aggregates,
            source_routes,
        )
    }
}

fn merge_manifest_sources(
    base: &[CertifiedSource],
    mut upserts: BTreeMap<[u8; 32], CertifiedSource>,
    deletions: &BTreeSet<[u8; 32]>,
) -> Vec<CertifiedSource> {
    let mut sources = Vec::with_capacity(base.len().saturating_add(upserts.len()));
    for certificate in base {
        #[cfg(test)]
        BASE_MANIFEST_SOURCE_MATERIALIZATIONS
            .with(|visits| visits.set(visits.get().saturating_add(1)));
        let digest = source_sort_key(certificate.observation().source());
        if let Some(replacement) = upserts.remove(&digest) {
            sources.push(replacement);
        } else if !deletions.contains(&digest) {
            sources.push(certificate.clone());
        }
    }
    sources.extend(upserts.into_values());
    sources
}

fn merge_partial_route_members(
    base: &[SourceKey],
    delta: &PartialSourceRouteDelta,
) -> Vec<SourceKey> {
    let mut upserts = delta.upserts.clone();
    let mut members = Vec::with_capacity(base.len().saturating_add(upserts.len()));
    for member in base {
        #[cfg(test)]
        PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS
            .with(|visits| visits.set(visits.get().saturating_add(1)));
        let digest = member.identity().digest();
        if let Some(replacement) = upserts.remove(&digest) {
            members.push(replacement);
        } else if !delta.deletions.contains(&digest) {
            members.push(member.clone());
        }
    }
    members.extend(upserts.into_values());
    members
}
