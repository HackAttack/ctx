use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use crate::{IndexError, Result};
use ctx_history_index_format::{
    is_generation_id, load_publication_for_metas, meta_generation, payload_generation_id,
    register_body_analyzer, scrub_and_certify_physical_integrity, searcher_generation,
    validate_schema, verify_or_certify_physical_integrity, verify_searcher,
    verify_searcher_structure, DurableMmapDirectory, GenerationManifest, VerifiedPublication,
};
use ctx_history_index_generation::{
    acquire_active_generation_read_lease, acquire_generation_read_lease,
    load_active_generation_pointer, open_slot_index, ActiveGenerationPointer,
    GenerationReadLeaseAcquisition, GenerationSlot,
};
use tantivy::{ReloadPolicy, Searcher};

#[cfg(any(test, feature = "test-support"))]
type BeforePeerLeaseHook = Box<dyn FnMut(usize)>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static VERIFIED_INDEX_REOPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VERIFIED_INDEX_AFTER_PUBLICATION_FENCE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static VERIFIED_INDEX_BEFORE_PEER_LEASE_HOOK: std::cell::RefCell<Option<BeforePeerLeaseHook>> = const { std::cell::RefCell::new(None) };
}

/// A verified reader pinned to one immutable lexical generation.
///
/// Feature-enabled downstream code cannot extract its raw Tantivy searcher or
/// index handle:
///
/// ```compile_fail
/// use ctx_history_index_query::VerifiedIndex;
///
/// fn expose_raw_searcher(index: &VerifiedIndex) {
///     let _ = index.test_searcher();
/// }
/// ```
pub struct VerifiedIndex {
    pub(crate) searcher: Searcher,
    pub(crate) manifest: Arc<GenerationManifest>,
    pub(crate) generation_id: String,
    pub(crate) publication_metadata: Option<Arc<[u8]>>,
    pub(crate) semantic_eligibility_postings: OnceLock<crate::SemanticEligibilityPostings>,
}

impl VerifiedIndex {
    /// Returns the generation named by the validated active pointer, commit
    /// payload, and current Core manifest contract.
    pub fn active_generation_id(root: impl AsRef<Path>) -> Result<Option<String>> {
        if !root.as_ref().is_dir() {
            return Ok(None);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        let acquisition = match acquire_active_generation_read_lease(&root) {
            Ok(acquisition) => acquisition,
            Err(ctx_history_index_generation::GenerationError::MissingActiveGenerationPointer) => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let slot = acquisition.target().clone();
        let index = open_slot_index(&root, &slot)?;
        let _lease = acquisition.release_publication_fence();
        let metas = index.load_metas()?;
        let generation_id =
            payload_generation_id(&metas)?.ok_or(IndexError::MissingCommitPayload)?;
        if generation_id != slot.generation_id() {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        let publication = load_publication_for_metas(&root, &metas)?;
        if publication.generation_id() != generation_id {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        Ok(Some(publication.generation_id().to_owned()))
    }

    /// Opens a generation, validates its durable physical identity
    /// certification (rehashing if it is unavailable or stale), and audits
    /// every stored Core record plus its source and identity aggregates.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), true, false)
    }

    /// Forces a complete physical SHA-256/CRC scrub and exhaustive stored-Core
    /// audit, then refreshes the durable identity certification.
    pub fn scrub(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), true, true)
    }

    /// Opens a previously audited immutable generation for querying.
    ///
    /// The pointer, manifest, generation payload, schema/policy contract,
    /// Tantivy generation pin, certified artifact identities, and total
    /// document count are verified on every open. Artifact bodies are rehashed
    /// only when the durable certification is unavailable or stale. The
    /// publication-time O(document-count) identity audit is not repeated for
    /// current generations.
    pub fn open_pinned(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref(), false, false)
    }

    /// Opens exactly the requested active or retained previous generation.
    ///
    /// Resolution and a shared read lease are acquired under the short
    /// generation-ownership fence. The lease keeps that exact immutable slot
    /// alive while publication resumes and the reader constructs its manifest
    /// and searcher.
    ///
    /// Like [`Self::open_pinned`], this performs reopen-time certified physical
    /// identity and structural verification of the selected manifest, payload,
    /// schema/policy contract, Tantivy generation pin, and total document
    /// count. It does not repeat the O(document-count) stored-Core identity and
    /// source audit for current generations.
    pub fn open_pinned_generation(
        root: impl AsRef<Path>,
        expected_generation_id: &str,
    ) -> Result<Self> {
        if !is_generation_id(expected_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        let acquisition = match acquire_generation_read_lease(root.as_ref(), expected_generation_id)
            .map_err(IndexError::from)
        {
            Ok(acquisition) => acquisition,
            Err(IndexError::GenerationRetentionLeaseTargetNotRetained { .. }) => {
                return Err(Self::pinned_generation_not_retained(
                    root.as_ref(),
                    expected_generation_id,
                )?);
            }
            Err(error) => return Err(error),
        };
        Self::open_leased_slot(acquisition, false, false, |actual_generation_id| {
            IndexError::PinnedGenerationMismatch {
                expected_generation_id: expected_generation_id.to_owned(),
                actual_generation_id,
            }
        })
    }

    /// Opens the one other generation retained beside an already pinned
    /// active or previous generation.
    ///
    /// Compact rendered references use this peer to remain unambiguous across
    /// one publication transition. Resolution is limited to the two slots in
    /// the active pointer and fails closed if the caller's pinned generation
    /// is no longer retained.
    pub fn open_retained_generation_peer(
        root: impl AsRef<Path>,
        pinned_generation_id: &str,
    ) -> Result<Option<Self>> {
        if !is_generation_id(pinned_generation_id) {
            return Err(IndexError::InvalidGenerationId);
        }
        if !root.as_ref().is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let control_directory =
            DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let root = control_directory.root_path().to_path_buf();
        #[cfg(any(test, feature = "test-support"))]
        let mut before_peer_lease_hook =
            VERIFIED_INDEX_BEFORE_PEER_LEASE_HOOK.with(|hook| hook.borrow_mut().take());

        for attempt in 0..2 {
            let pointer = load_active_generation_pointer(&root)?
                .ok_or(IndexError::MissingActiveGenerationPointer)?;
            let peer = Self::retained_generation_peer(&pointer, pinned_generation_id)?;
            let Some(peer) = peer else {
                return Ok(None);
            };
            let expected_peer_generation_id = peer.generation_id().to_owned();
            #[cfg(any(test, feature = "test-support"))]
            if let Some(hook) = before_peer_lease_hook.as_mut() {
                hook(attempt);
            }
            match acquire_generation_read_lease(&root, &expected_peer_generation_id)
                .map_err(IndexError::from)
            {
                Ok(acquisition) => {
                    let current_peer = Self::retained_generation_peer(
                        acquisition.pointer(),
                        pinned_generation_id,
                    )?;
                    let Some(current_peer) = current_peer else {
                        return Ok(None);
                    };
                    if current_peer.generation_id() != acquisition.target().generation_id() {
                        if attempt == 0 {
                            continue;
                        }
                        return Err(IndexError::ConcurrentGenerationChange);
                    }
                    return Self::open_leased_slot(
                        acquisition,
                        false,
                        false,
                        |actual_generation_id| IndexError::PinnedGenerationMismatch {
                            expected_generation_id: expected_peer_generation_id,
                            actual_generation_id,
                        },
                    )
                    .map(Some);
                }
                Err(IndexError::GenerationRetentionLeaseTargetNotRetained { .. }) => {
                    if attempt == 0 {
                        continue;
                    }
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                Err(error) => return Err(error),
            }
        }
        Err(IndexError::ConcurrentGenerationChange)
    }

    fn retained_generation_peer<'a>(
        pointer: &'a ActiveGenerationPointer,
        pinned_generation_id: &str,
    ) -> Result<Option<&'a GenerationSlot>> {
        if pointer.active().generation_id() == pinned_generation_id {
            return Ok(pointer.previous());
        }
        if pointer
            .previous()
            .is_some_and(|slot| slot.generation_id() == pinned_generation_id)
        {
            return Ok(Some(pointer.active()));
        }
        Err(IndexError::PinnedGenerationNotRetained {
            expected_generation_id: pinned_generation_id.to_owned(),
            active_generation_id: pointer.active().generation_id().to_owned(),
            previous_generation_id: pointer
                .previous()
                .map(|slot| slot.generation_id().to_owned()),
        })
    }

    fn open_inner(
        root: &Path,
        audit_stored_core: bool,
        force_physical_scrub: bool,
    ) -> Result<Self> {
        if !root.is_dir() {
            return Err(IndexError::MissingActiveGenerationPointer);
        }
        let acquisition = acquire_active_generation_read_lease(root).map_err(IndexError::from)?;
        Self::open_leased_slot(acquisition, audit_stored_core, force_physical_scrub, |_| {
            IndexError::InvalidActiveGenerationPointer
        })
    }

    fn pinned_generation_not_retained(
        root: &Path,
        expected_generation_id: &str,
    ) -> Result<IndexError> {
        let pointer = load_active_generation_pointer(root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        Ok(IndexError::PinnedGenerationNotRetained {
            expected_generation_id: expected_generation_id.to_owned(),
            active_generation_id: pointer.active().generation_id().to_owned(),
            previous_generation_id: pointer
                .previous()
                .map(|slot| slot.generation_id().to_owned()),
        })
    }

    fn open_leased_slot<F>(
        acquisition: GenerationReadLeaseAcquisition,
        audit_stored_core: bool,
        force_physical_scrub: bool,
        generation_mismatch: F,
    ) -> Result<Self>
    where
        F: FnOnce(String) -> IndexError,
    {
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_REOPEN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        let root = acquisition.root().to_path_buf();
        let pointer = acquisition.pointer().clone();
        let slot = acquisition.target().clone();
        let index = open_slot_index(&root, &slot)?;
        register_body_analyzer(&index);
        validate_schema(&index.schema())?;
        let metas = index.load_metas()?;
        let payload_generation =
            payload_generation_id(&metas)?.ok_or(IndexError::MissingCommitPayload)?;
        if slot.generation_id() != payload_generation {
            return Err(generation_mismatch(payload_generation));
        }
        if force_physical_scrub {
            scrub_and_certify_physical_integrity(&root, &pointer, &slot, &index)?;
        } else {
            verify_or_certify_physical_integrity(&root, &pointer, &slot, &index)?;
        }

        // Publication and reclamation may resume now. The shared lease keeps
        // this exact immutable generation alive during the expensive manifest
        // and searcher construction below.
        let _lease = acquisition.release_publication_fence();
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_AFTER_PUBLICATION_FENCE_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let publication = load_publication_for_metas(&root, &metas)?;
        let (generation_id, manifest, publication_metadata) = publication.into_parts();
        if slot.generation_id() != generation_id {
            return Err(generation_mismatch(generation_id));
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if audit_stored_core {
            verify_searcher(&searcher, &manifest)?;
        } else {
            verify_searcher_structure(&searcher, &manifest)?;
        }
        Ok(Self {
            searcher,
            manifest,
            generation_id,
            publication_metadata,
            semantic_eligibility_postings: OnceLock::new(),
        })
    }

    #[doc(hidden)]
    pub fn from_verified_publication(publication: VerifiedPublication) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT
            .with(|count| count.set(count.get().saturating_add(1)));
        let (searcher, manifest, generation_id, publication_metadata) = publication.into_parts();
        Self {
            searcher,
            manifest,
            generation_id,
            publication_metadata,
            semantic_eligibility_postings: OnceLock::new(),
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_shared_manifest(&self) -> &Arc<GenerationManifest> {
        &self.manifest
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_with_searcher(mut self, searcher: Searcher) -> Self {
        self.searcher = searcher;
        self.semantic_eligibility_postings = OnceLock::new();
        self
    }

    /// Returns refresh-owned opaque bytes bound to this exact generation's
    /// canonical Tantivy commit payload.
    pub fn publication_metadata(&self) -> Option<&[u8]> {
        self.publication_metadata.as_deref()
    }

    pub fn document_count(&self) -> u64 {
        self.searcher.num_docs()
    }

    pub fn validate_checksums(&self) -> Result<HashSet<PathBuf>> {
        Ok(self.searcher.index().validate_checksum()?)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn count_term(&self, term_text: &str) -> Result<usize> {
        use tantivy::{collector::Count, query::TermQuery, schema::IndexRecordOption, Term};

        let body = ctx_history_index_format::required_field(self.searcher.schema(), "body_search")?;
        let query = TermQuery::new(
            Term::from_field_text(body, term_text),
            IndexRecordOption::Basic,
        );
        Ok(self.searcher.search(&query, &Count)?)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_verified_index_reopen_count() {
    VERIFIED_INDEX_REOPEN_COUNT.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn verified_index_reopen_count() -> usize {
    VERIFIED_INDEX_REOPEN_COUNT.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_verified_index_publication_construction_count() {
    VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT.with(|count| count.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn verified_index_publication_construction_count() -> usize {
    VERIFIED_INDEX_PUBLICATION_CONSTRUCTION_COUNT.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_verified_index_after_publication_fence_hook(hook: impl FnOnce() + 'static) {
    VERIFIED_INDEX_AFTER_PUBLICATION_FENCE_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "verified-index publication-fence hook already installed"
        );
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_verified_index_before_peer_lease_hook(hook: impl FnMut(usize) + 'static) {
    VERIFIED_INDEX_BEFORE_PEER_LEASE_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "verified-index peer-lease hook already installed"
        );
    });
}
