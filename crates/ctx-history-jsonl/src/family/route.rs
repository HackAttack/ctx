use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::{
    observe_opened_file, revalidate_frozen_prefix, JsonlCheckpoint, JsonlFileObservation,
    JsonlOversizedRecordPolicy, JsonlPhysicalEncoding, JsonlProbe, JsonlReader, JsonlRecordFraming,
    JsonlRecordRef, OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
    ProviderSourceRoot,
};
use super::{
    JsonlFamilyError, JsonlFamilyRuntime, JsonlResult, JsonlRuntimeError, JsonlRuntimeLookup,
};
use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::SourceBackedRouteErrorKind;
use ctx_history_capture_runtime::{
    CaptureLifecycleSink, ImmutableCaptureSnapshot, SourceBackedGenerationSink,
    SourceBackedRecordRejectionDrafts, SourceBackedRevalidationTarget, SourceBackedRouteError,
    SourceBackedRouteResult,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, ProjectionContractError, SourceFrontier, SourceInventoryObservation, SourceKey,
    TypedKey,
};
use ctx_history_source_io::{
    open_provider_source_path_mapped as open_provider_source_path,
    PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FAMILY_POLICY_REVISION: &str = "borrowed-jsonl-certified-append-v1";
const FAMILY_FRONTIER_KIND: &str = "borrowed-jsonl-family-checkpoint-v1";
const FAMILY_SOURCE_REVISION_KIND: &str = "borrowed-jsonl-file-observation-v1";
const FAMILY_INVENTORY_AUTHORITY: &str = "borrowed-jsonl-provider-root-v1";
const FAMILY_INVENTORY_REVISION: &str = "borrowed-jsonl-inventory-v1";
const FAMILY_DISCOVERY_REVISION: &str = "borrowed-jsonl-discovery-v1";
const FAMILY_INVENTORY_DOMAIN: &[u8] = b"ctx-borrowed-jsonl-inventory-v1\0";
type JsonlSemanticExecutorResult<R> =
    JsonlResult<Option<Box<dyn JsonlFamilySemanticExecutor<Runtime = R>>>, JsonlRuntimeError<R>>;
type JsonlOptimizedLeafResult<R> = JsonlResult<
    Option<JsonlFamilyOptimizedLeafOutcome<JsonlRuntimeError<R>>>,
    JsonlRuntimeError<R>,
>;
mod leaf;
#[cfg(any(test, feature = "test-support"))]
pub use leaf::checkpoint_admitted_revision_for_test;
#[cfg(test)]
use leaf::family_scanner_worker_count_policy;
use leaf::{base_for_leaf, decode_checkpoint, scan_leaves};
#[cfg(test)]
use leaf::{prepare_leaf, JsonlLeafOutput, JsonlLeafOutputEvent};
mod errors;
use errors::{
    contract_error, normalized_jsonl_error_kind, route_discovery, route_internal, route_invalid,
    route_scan,
};
mod ownership;
use ownership::base_sources_for_root;
mod revalidation;
#[cfg(any(test, feature = "test-support"))]
pub use revalidation::set_before_jsonl_terminal_physical_revalidation_hook;
use revalidation::{
    binding_digest, inventory_observation, reset_terminal, revalidate_complete_inventory,
    revalidate_target,
};
mod scanner;
#[cfg(test)]
pub(crate) use scanner::with_family_scanner_workers;
#[cfg(test)]
use scanner::{
    jsonl_family_scanner_activity, jsonl_family_scanner_probe,
    record_jsonl_family_scanner_activity, JsonlFamilyScannerActivity, JsonlFamilyScannerProbe,
    FAMILY_SCANNER_WORKERS_OVERRIDE,
};
use scanner::{physical_identity, source_observation};
pub use scanner::{
    JsonlFamilyAppendMode, JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode, JsonlFamilyPublication,
    JsonlFamilySemanticPage, JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary,
    JsonlFamilyWorkerContext,
};
mod terminal;
pub use terminal::JsonlFamilyTerminalProof;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyRootMissingMode {
    /// A missing provider-owned root is not evidence that every prior source
    /// was deleted; leave the route unavailable.
    Unavailable,
    /// One explicitly registered authority disappeared. Certify an empty
    /// inventory so the shared family can delete its formerly owned sources.
    AuthoritativeEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyInventoryMode {
    /// The complete discovered tree must remain byte-for-byte identical from
    /// opening through terminal revalidation.
    Exact,
    /// The opening membership is the generation boundary. Captured members
    /// must retain their certified ordinary-file prefixes, deleted members
    /// must remain absent, and newly discovered members are deferred to the
    /// next refresh.
    FrozenOpeningAllowAdditions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyBaseScope {
    /// Compatibility mode for family adapters whose source identity is unique
    /// across every route for that provider/schema tuple.
    ProviderFamily,
    /// Reuse only sources previously committed by this exact route. Adapters
    /// whose explicit and automatic routes can overlap must select this mode.
    Route,
}

pub trait JsonlFamilyProjector: Send {
    type Runtime: JsonlFamilyRuntime;

    fn preflight(
        &mut self,
        _reader: &mut JsonlReader<JsonlRuntimeError<Self::Runtime>>,
        _certified_prefix_end: Option<u64>,
    ) -> JsonlResult<bool, JsonlRuntimeError<Self::Runtime>> {
        Ok(false)
    }

    fn retry_replacement(&mut self) {}

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>;

    fn finish(&mut self) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        _emit: &mut dyn FnMut(CoreRecord) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        self.finish()
    }

    fn rejected_records(&self) -> u64 {
        0
    }

    /// Opaque, contract-bounded provider state to carry into the next certified
    /// suffix projection. The family persists the value without interpreting it.
    fn provider_checkpoint(
        &self,
    ) -> JsonlResult<Option<TypedKey>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }
}

pub trait JsonlFamilySemanticExecutor: Send {
    type Runtime: JsonlFamilyRuntime;

    /// Runs before writer staging or record emission. Append executors may ask
    /// the family to reopen and retry this leaf once as a replacement.
    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<Self::Runtime>,
    ) -> JsonlResult<JsonlFamilySemanticPreflight, JsonlRuntimeError<Self::Runtime>>;

    /// Produces one bounded semantic page from shared-owned physical input.
    /// Returning `None` means the input is exhausted and no page remains.
    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<Self::Runtime>,
        worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
    ) -> JsonlResult<Option<JsonlFamilySemanticPage>, JsonlRuntimeError<Self::Runtime>>;

    /// Returns only semantic classification and opaque continuation state.
    fn finish(
        self: Box<Self>,
    ) -> JsonlResult<JsonlFamilySemanticSummary, JsonlRuntimeError<Self::Runtime>>;
}

pub trait JsonlFamilyAdapter: Send + Sync {
    type Runtime: JsonlFamilyRuntime;

    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &'static str;
    fn schema_variant(&self) -> &'static str;
    fn parser_revision(&self) -> &'static str;
    /// Projection-local identity scheme revision. Changing this invalidates
    /// the family checkpoint and forces a replacement scan without changing
    /// the provider parser revision recorded by Core.
    fn event_identity_revision(&self) -> &'static str {
        ""
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode;

    /// Selects the physical record framing for ordinary JSONL leaves. The
    /// family copies this policy into the reader once when the leaf opens;
    /// whole-record leaves retain their separate exact-file behavior.
    fn record_framing(&self) -> JsonlRecordFraming {
        JsonlRecordFraming::ordinary()
    }

    /// Selects the bounded physical units owned by the shared reader. Raw
    /// JSONL remains the compatibility default; adapters may select
    /// concatenated checksummed Zstandard frames per leaf.
    fn physical_encoding(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlPhysicalEncoding {
        JsonlPhysicalEncoding::RawJsonl
    }

    /// Binds the complete admitted EOF, including an unfinished tail, with a
    /// raw SHA-256 digest owned and revalidated by the shared family.
    fn bind_admitted_eof(&self) -> bool {
        false
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectSource
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::Unavailable
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::Exact
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::ProviderFamily
    }

    fn discover(
        &self,
        root: &Path,
    ) -> JsonlResult<
        JsonlFamilyInventory<JsonlRuntimeError<Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    >;

    /// Observes only physical route membership. Implementations must not parse
    /// identities or hash transcript bodies; content authority belongs to the
    /// task-local terminal proofs returned by leaf scans.
    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<
        JsonlFamilyMembershipObservation<JsonlRuntimeError<Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn discovery_error_kind(
        &self,
        _error: &JsonlRuntimeError<Self::Runtime>,
    ) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    fn scan_error_kind(
        &self,
        _error: &JsonlRuntimeError<Self::Runtime>,
    ) -> SourceBackedRouteErrorKind {
        SourceBackedRouteErrorKind::InvalidSource
    }

    /// Applies a deterministic provider-declared dependency order before the
    /// shared family scheduler starts any leaf workers. Adapters may reorder
    /// the supplied leaves but must not add or remove them.
    fn order_leaf_scans(
        &self,
        _leaves: &mut [JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>],
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Performs adapter-owned preparation that must complete before any leaf
    /// worker starts and may conservatively cap this capture's worker count.
    /// The default has no preparation and keeps the shared scheduler budget.
    fn prepare_leaf_scans(
        &self,
        _leaves: &[JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>],
        _bases: &HashMap<[u8; 32], &CertifiedSource>,
    ) -> JsonlResult<Option<usize>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Returns the dependency phase for one leaf after `prepare_leaf_scans`.
    /// The shared scheduler runs every leaf in a phase concurrently, joins all
    /// of those workers, and only then starts the next phase. Adapters that use
    /// this hook must order leaves by nondecreasing phase. The default keeps
    /// every leaf in one fully parallel phase.
    fn leaf_scan_phase(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<usize, JsonlRuntimeError<Self::Runtime>> {
        Ok(0)
    }

    /// Returns an independent dependency partition for one leaf. When every
    /// selected leaf has a partition, the shared scheduler admits a bounded
    /// wave of partitions and runs each dependency-phase frontier across that
    /// wave on fixed logical cache lanes. Partition-local adapter state remains
    /// live from the begin hook through the matching finish hook.
    fn leaf_scan_partition(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<Option<u64>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Conservatively narrows the shared maximum of 16 simultaneously live
    /// dependency partitions. Adapters may lower but never raise the shared
    /// ceiling; returning zero is invalid.
    fn leaf_scan_partition_wave_limit(&self) -> usize {
        16
    }

    /// Prepares partition-local state immediately before its first leaf runs.
    fn begin_leaf_scan_partition(
        &self,
        _partition: u64,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Releases partition-local state after all of its leaves have joined.
    fn finish_leaf_scan_partition(
        &self,
        _partition: u64,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    /// Pins unpartitioned leaves to one persistent worker-state slot across
    /// dependency phases. Partitioned scans use size-balanced frontier lanes
    /// instead. Equal affinities must denote leaves that may safely serialize
    /// on one worker; the default leaves assignment round-robin.
    fn leaf_worker_affinity(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<Option<u64>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Releases adapter-owned scan-only state after all leaf workers have
    /// joined. Terminal source and inventory revalidation must keep only the
    /// evidence they need beyond this boundary.
    fn finish_leaf_scans(&self) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _source_file: Arc<OpenedProviderSourceFile<JsonlRuntimeError<Self::Runtime>>>,
        _imported_at: DateTime<Utc>,
    ) -> JsonlResult<
        Box<dyn JsonlFamilyProjector<Runtime = Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        Err(JsonlRuntimeError::<Self::Runtime>::system_invariant(
            "missing JSONL projector",
        ))
    }

    /// Constructs a projector for a cold/replacement scan or from the opaque
    /// provider state persisted at the validated prefix frontier. Any scan with
    /// an exact prior source receives an event-identity lookup pinned to the
    /// writer base. `mode` distinguishes append continuation from replacement
    /// reconciliation; cold scans receive no lookup.
    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        source_file: Arc<OpenedProviderSourceFile<JsonlRuntimeError<Self::Runtime>>>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<JsonlRuntimeLookup<Self::Runtime>>,
        _mode: JsonlFamilyProjectionMode,
    ) -> JsonlResult<
        Box<dyn JsonlFamilyProjector<Runtime = Self::Runtime>>,
        JsonlRuntimeError<Self::Runtime>,
    > {
        if checkpoint.is_some() {
            return Err(JsonlRuntimeError::<Self::Runtime>::invalid_payload(
                "JSONL adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }

    /// Optional bounded semantic executor. The family has already selected the
    /// physical projection mode and retains all lifecycle/publication authority.
    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<JsonlRuntimeLookup<Self::Runtime>>,
        _mode: JsonlFamilyProjectionMode,
    ) -> JsonlSemanticExecutorResult<Self::Runtime> {
        Ok(None)
    }

    /// Removes one unit of provider-declared optional checkpoint evidence.
    /// The shared family calls this only when the completed FamilyCheckpoint
    /// fails the real SourceFrontier typed-key contract. Durable provider
    /// authority must never be removed by this hook.
    fn shed_optional_provider_checkpoint_evidence(
        &self,
        _checkpoint: &TypedKey,
    ) -> JsonlResult<Option<TypedKey>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }

    /// Legacy optimized execution retained for adapters outside the Codex
    /// convergence tranche. New adapters must use `semantic_executor`.
    fn scan_optimized_leaf(
        &self,
        _leaf: &JsonlFamilyLeaf<JsonlRuntimeError<Self::Runtime>>,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &JsonlRuntimeLookup<Self::Runtime>,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        _emit_page: &mut dyn FnMut(
            JsonlFamilyPublication,
            u64,
            Vec<CoreRecord>,
        ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlOptimizedLeafResult<Self::Runtime> {
        Ok(None)
    }

    /// Resolves the ordinary path represented by a committed base. Optimized
    /// adapters with their own bounded frontier format may override this; the
    /// default decodes the shared family checkpoint.
    fn base_source_path(
        &self,
        certificate: &CertifiedSource,
    ) -> JsonlResult<PathBuf, JsonlRuntimeError<Self::Runtime>> {
        default_base_source_path(self, certificate)
    }

    fn owns(&self, source: &SourceKey) -> bool {
        source.provider() == self.provider().as_str()
            && source.source_format() == self.source_format()
            && source.schema_variant() == self.schema_variant()
            && source.provider_identity_version() == 1
    }
}

/// Content-free physical membership observed at admission or at the terminal
/// fence. Source hints are optional and are used only to recognize a deleted
/// logical source that reappears at a new physical route under frozen mode.
#[derive(Debug)]
pub struct JsonlFamilyMembershipObservation<E: JsonlFamilyError> {
    root_missing: bool,
    routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
    source_hints: HashMap<PathBuf, SourceKey>,
}

#[derive(Debug)]
struct JsonlFamilyMembershipRoute<E: JsonlFamilyError> {
    authority: Arc<ProviderSourceRoot<E>>,
    authority_path: PathBuf,
}

impl<E: JsonlFamilyError> JsonlFamilyMembershipObservation<E> {
    pub fn observe(root: &Path, opening: &JsonlFamilyInventory<E>) -> JsonlResult<Self, E> {
        if opening.root_missing {
            return match open_provider_source_path::<E>(root) {
                Err(error) if error.is_not_found() => Ok(Self {
                    root_missing: true,
                    routes: BTreeMap::new(),
                    source_hints: HashMap::new(),
                }),
                Ok(_) => Err(E::source_changed()),
                Err(error) => Err(error),
            };
        }

        let absolute_root = std::path::absolute(root)?;
        if let Some(leaf) = opening
            .leaves
            .iter()
            .find(|leaf| leaf.source_path == absolute_root)
        {
            return Self::observe_leaf(leaf, opening);
        }
        Self::observe_authorities(opening)
    }

    pub fn observe_authorities(opening: &JsonlFamilyInventory<E>) -> JsonlResult<Self, E> {
        let mut state = JsonlFamilyMembershipState::default();
        for authority in &opening.authorities {
            let directory = authority.directory()?;
            observe_membership_directory(&directory, 0, &mut state)?;
            authority.revalidate_same_object()?;
        }
        Self::from_routes(state.routes, opening)
    }

    fn observe_leaf(
        leaf: &JsonlFamilyLeaf<E>,
        opening: &JsonlFamilyInventory<E>,
    ) -> JsonlResult<Self, E> {
        check_membership_path::<E>(&leaf.source_path)?;
        if leaf.authority_path.components().count()
            > PROVIDER_JSONL_INVENTORY_MAX_DEPTH.saturating_add(1)
        {
            return Err(E::invalid_payload(
                "JSONL membership path depth exceeds the provider inventory bound".to_owned(),
            ));
        }
        let opened = leaf.authority.open_file(&leaf.authority_path)?;
        opened.revalidate_same_object()?;
        let mut routes = BTreeMap::new();
        routes.insert(
            leaf.source_path.clone(),
            JsonlFamilyMembershipRoute {
                authority: Arc::clone(&leaf.authority),
                authority_path: leaf.authority_path.clone(),
            },
        );
        Self::from_routes(routes, opening)
    }

    fn from_routes(
        routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
        opening: &JsonlFamilyInventory<E>,
    ) -> JsonlResult<Self, E> {
        let source_hints = opening
            .leaves
            .iter()
            .filter(|leaf| routes.contains_key(&leaf.source_path))
            .map(|leaf| (leaf.source_path.clone(), leaf.source.clone()))
            .collect();
        Ok(Self {
            root_missing: false,
            routes,
            source_hints,
        })
    }

    pub fn unbound_routes(
        &self,
    ) -> impl Iterator<Item = (&Path, Arc<ProviderSourceRoot<E>>, &Path)> {
        self.routes
            .iter()
            .filter(|(path, _)| !self.source_hints.contains_key(*path))
            .map(|(path, route)| {
                (
                    path.as_path(),
                    Arc::clone(&route.authority),
                    route.authority_path.as_path(),
                )
            })
    }

    pub fn bind_source_hint(&mut self, path: PathBuf, source: SourceKey) {
        if self.routes.contains_key(&path) {
            self.source_hints.insert(path, source);
        }
    }

    fn admits(
        &self,
        current: &Self,
        mode: JsonlFamilyInventoryMode,
        expected_sources: &HashMap<[u8; 32], TerminalSourceEvidence<E>>,
        owned_sources: &HashMap<[u8; 32], SourceKey>,
    ) -> bool {
        if self.root_missing != current.root_missing {
            return false;
        }
        match mode {
            JsonlFamilyInventoryMode::Exact => self.routes.keys().eq(current.routes.keys()),
            JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => {
                current.source_hints.values().all(|source| {
                    let digest = source.exact_descriptor_digest();
                    !owned_sources
                        .get(&digest)
                        .is_some_and(|owned| owned.exact_descriptor_eq(source))
                        || expected_sources.contains_key(&digest)
                })
            }
        }
    }
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyMembershipRoute<E> {
    fn clone(&self) -> Self {
        Self {
            authority: Arc::clone(&self.authority),
            authority_path: self.authority_path.clone(),
        }
    }
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyMembershipObservation<E> {
    fn clone(&self) -> Self {
        Self {
            root_missing: self.root_missing,
            routes: self.routes.clone(),
            source_hints: self.source_hints.clone(),
        }
    }
}

struct JsonlFamilyMembershipState<E: JsonlFamilyError> {
    directories: usize,
    entries: usize,
    routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
}

impl<E: JsonlFamilyError> Default for JsonlFamilyMembershipState<E> {
    fn default() -> Self {
        Self {
            directories: 0,
            entries: 0,
            routes: BTreeMap::new(),
        }
    }
}

fn observe_membership_directory<E: JsonlFamilyError>(
    directory: &ProviderSourceDirectory<E>,
    depth: usize,
    state: &mut JsonlFamilyMembershipState<E>,
) -> JsonlResult<(), E> {
    if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
        return Err(E::invalid_payload(
            "JSONL membership directory depth exceeds the provider inventory bound".to_owned(),
        ));
    }
    state.directories = state.directories.saturating_add(1);
    if state.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
        return Err(E::invalid_payload(
            "JSONL membership directory count exceeds the provider inventory bound".to_owned(),
        ));
    }

    // Bound enumeration before the platform helper allocates the child list.
    let remaining = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
        .checked_sub(state.entries)
        .ok_or_else(|| {
            E::invalid_payload(
                "JSONL membership entry count exceeds the provider inventory bound".to_owned(),
            )
        })?;
    let children = directory.entries(remaining)?;
    state.entries = state
        .entries
        .checked_add(children.len())
        .ok_or_else(|| E::invalid_payload("JSONL membership entry count overflowed".to_owned()))?;

    for name in children {
        let authority_path = directory.relative_path().join(&name);
        let authority = directory.authority_root();
        let source_path = authority.named_path().join(&authority_path);
        check_membership_path::<E>(&source_path)?;
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            // Admission never admits a link-like or non-regular route (a
            // selected transcript that is a link fails admission), so
            // skipping here only ever drops non-route entries or a route that
            // changed into a link after admission; that change drops out of
            // the observed route set and fails the membership comparison as a
            // source change.
            Err(error) if error.is_ignorable_membership_entry() => {
                continue;
            }
            Err(error) => return Err(error),
        };
        match opened {
            OpenedProviderSourcePath::Directory(child) => {
                observe_membership_directory(&child, depth.saturating_add(1), state)?;
            }
            OpenedProviderSourcePath::File(opened)
                if source_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "json" | "jsonl"))
                    || source_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".jsonl.zstd")) =>
            {
                opened.revalidate_same_object_leaf()?;
                if state
                    .routes
                    .insert(
                        source_path,
                        JsonlFamilyMembershipRoute {
                            authority: Arc::new(authority),
                            authority_path,
                        },
                    )
                    .is_some()
                {
                    return Err(E::invalid_payload(
                        "JSONL membership contains a duplicate authority route".to_owned(),
                    ));
                }
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    // The root directory capability predates admission, so its exact metadata
    // stamp legitimately changes when frozen-mode writers add or remove a
    // child. The retained authority fence below proves root identity; exact
    // inventories additionally compare the root's full admission stamp before
    // and after this walk. Descendant directories were opened by this walk and
    // can therefore use an exact enumeration fence.
    if depth > 0 {
        directory.revalidate()?;
    }
    Ok(())
}

fn check_membership_path<E: JsonlFamilyError>(path: &Path) -> JsonlResult<(), E> {
    if path.as_os_str().as_encoded_bytes().len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
        return Err(E::invalid_payload(
            "JSONL membership path exceeds the provider inventory bound".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct JsonlFamilyLeaf<E: JsonlFamilyError> {
    source: SourceKey,
    source_path: PathBuf,
    authority_path: PathBuf,
    authority: Arc<ProviderSourceRoot<E>>,
    observation: JsonlFileObservation,
    binding: TypedKey,
    identity_probe: Option<JsonlProbe>,
    identity_probe_rejected_records: u64,
    whole_record: bool,
    freeze_observation_at_scan: bool,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyLeaf<E> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            source_path: self.source_path.clone(),
            authority_path: self.authority_path.clone(),
            authority: Arc::clone(&self.authority),
            observation: self.observation.clone(),
            binding: self.binding.clone(),
            identity_probe: self.identity_probe.clone(),
            identity_probe_rejected_records: self.identity_probe_rejected_records,
            whole_record: self.whole_record,
            freeze_observation_at_scan: self.freeze_observation_at_scan,
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyLeaf<E> {
    /// Binds admission to a descriptor already retained by an optimized
    /// adapter. The adapter may keep the same descriptor for its scan, avoiding
    /// a pathname reopen between shared leaf admission and provider parsing.
    pub fn bind_opened(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        opened: &OpenedProviderSourceFile<E>,
    ) -> JsonlResult<Self, E> {
        let observation = observe_opened_file(&source_path, opened)?;
        Ok(Self::bind_observed(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            observation,
        ))
    }

    pub fn bind_observed(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        observation: JsonlFileObservation,
    ) -> Self {
        Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record: false,
            freeze_observation_at_scan: false,
        }
    }

    pub fn bind_frozen_observed(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        observation: JsonlFileObservation,
    ) -> Self {
        Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record: false,
            freeze_observation_at_scan: true,
        }
    }

    pub fn observe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> JsonlResult<Self, E> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            false,
        )
    }

    pub fn observe_whole_record(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> JsonlResult<Self, E> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            true,
        )
    }

    pub fn observe_after_identity_probe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        mut identity_probe: JsonlProbe,
        identity_probe_rejected_records: u64,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        if observation != identity_probe.observation {
            revalidate_frozen_prefix(
                &source_path,
                &opened,
                &identity_probe.observation,
                identity_probe.complete_prefix_end,
                super::prefix_digest(&identity_probe.prefix_hasher),
            )?;
            identity_probe.observation = observation.clone();
        }
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: Some(identity_probe),
            identity_probe_rejected_records,
            whole_record: false,
            freeze_observation_at_scan: false,
        })
    }

    fn observe_with_framing(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        whole_record: bool,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            binding,
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record,
            freeze_observation_at_scan: false,
        })
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn authority(&self) -> &Arc<ProviderSourceRoot<E>> {
        &self.authority
    }

    pub fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }

    pub(super) fn frozen_scan_observation(&self) -> Option<&JsonlFileObservation> {
        self.freeze_observation_at_scan.then_some(&self.observation)
    }

    pub(super) fn estimated_scan_bytes(&self) -> u64 {
        self.observation.length()
    }

    pub fn binding(&self) -> &TypedKey {
        &self.binding
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_verified(&self) -> JsonlResult<Arc<OpenedProviderSourceFile<E>>, E> {
        let opened = self.authority.open_file(&self.authority_path)?;
        if observe_opened_file(&self.source_path, &opened)? != self.observation {
            return Err(E::source_changed());
        }
        Ok(Arc::new(opened))
    }

    fn open_for_scan(&self) -> JsonlResult<(Self, Arc<OpenedProviderSourceFile<E>>), E> {
        let opened = self.authority.open_file(&self.authority_path)?;
        let current = observe_opened_file(&self.source_path, &opened)?;
        if current == self.observation {
            return Ok((self.clone(), Arc::new(opened)));
        }
        if self.whole_record
            || current.length() <= self.observation.length()
            || !self.observation.admits_frozen_prefix_in(&current)
        {
            return Err(E::source_changed());
        }
        if self.freeze_observation_at_scan {
            return Ok((self.clone(), Arc::new(opened)));
        }
        let mut leaf = self.clone();
        leaf.observation = current.clone();
        if let Some(probe) = leaf.identity_probe.as_mut() {
            revalidate_frozen_prefix(
                &leaf.source_path,
                &opened,
                &probe.observation,
                probe.complete_prefix_end,
                super::prefix_digest(&probe.prefix_hasher),
            )?;
            probe.observation = current;
        }
        Ok((leaf, Arc::new(opened)))
    }
}

#[derive(Debug, Clone)]
pub struct JsonlFamilyRejectedLeaf {
    source_path: PathBuf,
    authority_path: PathBuf,
    proof: TypedKey,
    rejected_records: u64,
}

impl JsonlFamilyRejectedLeaf {
    pub fn bind_observed(
        source_path: PathBuf,
        authority_path: PathBuf,
        proof: TypedKey,
        rejected_records: u64,
    ) -> Self {
        Self {
            source_path,
            authority_path,
            proof,
            rejected_records,
        }
    }
}

#[derive(Debug)]
pub struct JsonlFamilyInventory<E: JsonlFamilyError> {
    root_missing: bool,
    observation: SourceInventoryObservation,
    authorities: Vec<Arc<ProviderSourceRoot<E>>>,
    leaves: Vec<JsonlFamilyLeaf<E>>,
    rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyInventory<E> {
    fn clone(&self) -> Self {
        Self {
            root_missing: self.root_missing,
            observation: self.observation.clone(),
            authorities: self.authorities.clone(),
            leaves: self.leaves.clone(),
            rejected_leaves: self.rejected_leaves.clone(),
            exact_dependencies: self.exact_dependencies.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyInventory<E> {
    pub fn present(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_with_rejected(provider, root, authority, leaves, Vec::new())
    }

    pub fn present_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, vec![authority], leaves, rejected_leaves)
    }

    pub fn present_multi(
        provider: CaptureProvider,
        root: &Path,
        authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, authorities, leaves, Vec::new())
    }

    pub fn present_multi_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        mut authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        mut leaves: Vec<JsonlFamilyLeaf<E>>,
        mut rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        if authorities.is_empty() {
            return Err(E::invalid_payload(
                "present JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        authorities.sort_by(|left, right| left.named_path().cmp(right.named_path()));
        for pair in authorities.windows(2) {
            if pair[0].named_path() == pair[1].named_path() {
                return Err(E::invalid_payload(format!(
                    "present JSONL inventory has duplicate root authority {}",
                    pair[0].named_path().display()
                )));
            }
        }
        for leaf in &leaves {
            let retained = authorities.iter().any(|authority| {
                authority.named_path() == leaf.authority.named_path()
                    && authority.authority_fingerprint() == leaf.authority.authority_fingerprint()
            });
            if !retained {
                return Err(E::invalid_payload(format!(
                    "JSONL leaf {} is outside the retained root authorities",
                    leaf.source_path.display()
                )));
            }
        }
        leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        rejected_leaves.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let observation = inventory_observation(
            provider,
            root,
            false,
            &authorities,
            &leaves,
            &rejected_leaves,
        )?;
        Ok(Self {
            root_missing: false,
            observation,
            authorities,
            leaves,
            rejected_leaves,
            exact_dependencies: Vec::new(),
        })
    }

    pub fn missing(provider: CaptureProvider, root: &Path) -> JsonlResult<Self, E> {
        Ok(Self {
            root_missing: true,
            observation: inventory_observation::<E>(provider, root, true, &[], &[], &[])?,
            authorities: Vec::new(),
            leaves: Vec::new(),
            rejected_leaves: Vec::new(),
            exact_dependencies: Vec::new(),
        })
    }

    pub fn with_exact_dependencies(
        mut self,
        exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
    ) -> Self {
        self.exact_dependencies = exact_dependencies;
        self
    }

    pub fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub fn leaves(&self) -> &[JsonlFamilyLeaf<E>] {
        &self.leaves
    }

    pub fn rejected_leaves(&self) -> &[JsonlFamilyRejectedLeaf] {
        &self.rejected_leaves
    }

    #[cfg(test)]
    fn certify_against(&self, closing: &Self) -> JsonlResult<CertifiedSourceInventory, E> {
        self.certify_selected_against(
            closing,
            closing
                .leaves
                .iter()
                .map(|leaf| leaf.source.clone())
                .collect(),
        )
    }

    fn certify_selected_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> JsonlResult<CertifiedSourceInventory, E> {
        if self.root_missing != closing.root_missing {
            return Err(E::invalid_payload(
                "JSONL root availability changed during capture".to_owned(),
            ));
        }
        CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            FAMILY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(contract_error)
    }

    fn revalidate_root(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate()?;
        }
        Ok(())
    }

    fn revalidate_root_same_object(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate_same_object()?;
        }
        Ok(())
    }

    fn revalidate_terminal_root(
        &self,
        root: &Path,
        mode: JsonlFamilyInventoryMode,
    ) -> JsonlResult<(), E> {
        if self.root_missing {
            return match open_provider_source_path::<E>(root) {
                Err(error) if error.is_not_found() => Ok(()),
                Ok(_) => Err(E::source_changed()),
                Err(error) => Err(error),
            };
        }
        match mode {
            JsonlFamilyInventoryMode::Exact => self.revalidate_root(),
            JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => {
                self.revalidate_root_same_object()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FamilyCheckpoint {
    version: u32,
    provider_parser_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    event_identity_revision: String,
    binding_digest: [u8; 32],
    physical: JsonlCheckpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admitted_eof_sha256: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "is_false")]
    complete_prefix_ends_with_terminal_nul_padding: bool,
    represented_physical_records: u64,
    rejected_records: u64,
    #[serde(default)]
    logical_complete_records: u64,
    #[serde(default)]
    rejected_logical_records: u64,
    indexed_documents: u64,
    provider_checkpoint: Option<TypedKey>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl FamilyCheckpoint {
    const VERSION: u32 = 5;

    fn encode_frontier_key<E: JsonlFamilyError>(&self) -> JsonlResult<TypedKey, E> {
        TypedKey::utf8(serde_json::to_string(self)?)
            .map_err(|error| E::invalid_payload(error.to_string()))
    }

    fn decode_frontier_key<E: JsonlFamilyError>(key: &TypedKey) -> JsonlResult<Self, E> {
        match key {
            // Bytes was emitted before the compact UTF-8 representation. Both
            // carry the same versioned JSON document and remain readable.
            TypedKey::Bytes(bytes) => Ok(serde_json::from_slice(bytes)?),
            TypedKey::Utf8(json) => Ok(serde_json::from_str(json)?),
            _ => Err(E::invalid_payload(
                "JSONL base checkpoint is malformed".to_owned(),
            )),
        }
    }

    fn fits_frontier_key<E: JsonlFamilyError>(&self) -> JsonlResult<bool, E> {
        let json = serde_json::to_string(self)?;
        let key = match TypedKey::utf8(json) {
            Ok(key) => key,
            Err(ProjectionContractError::FieldTooLarge { .. }) => return Ok(false),
            Err(error) => return Err(E::invalid_payload(error.to_string())),
        };
        match SourceFrontier::new(
            FAMILY_FRONTIER_KIND,
            key,
            self.physical.complete_prefix_end(),
            *self.physical.complete_prefix_sha256(),
        ) {
            Ok(_) => Ok(true),
            Err(ProjectionContractError::FieldTooLarge { .. }) => Ok(false),
            Err(error) => Err(E::invalid_payload(error.to_string())),
        }
    }

    fn valid_for<R: JsonlFamilyRuntime>(
        &self,
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    ) -> bool {
        self.version == Self::VERSION
            && self.provider_parser_revision == adapter.parser_revision()
            && self.event_identity_revision == adapter.event_identity_revision()
            && binding_digest(leaf).is_ok_and(|digest| self.binding_digest == digest)
            && self.physical.is_internally_consistent()
            && self.physical.identity() == &physical_identity(adapter, leaf)
            && (self.admitted_eof_sha256.is_some() == adapter.bind_admitted_eof())
            && self
                .provider_checkpoint
                .as_ref()
                .is_none_or(|checkpoint| checkpoint.validate_contract().is_ok())
            && self
                .represented_physical_records
                .checked_add(self.rejected_records)
                .is_some_and(|classified| classified <= self.physical.next_physical_ordinal())
            && self
                .indexed_documents
                .checked_add(self.rejected_logical_records)
                .is_some_and(|classified| classified <= self.logical_complete_records)
    }
}

#[derive(Debug)]
struct TerminalSourceEvidence<E: JsonlFamilyError> {
    certificate: CertifiedSource,
    terminal_proof: JsonlFamilyTerminalProof<E>,
    emitted_bytes: u64,
    record_rejections: SourceBackedRecordRejectionDrafts,
}

impl<E: JsonlFamilyError> Clone for TerminalSourceEvidence<E> {
    fn clone(&self) -> Self {
        Self {
            certificate: self.certificate.clone(),
            terminal_proof: self.terminal_proof.clone(),
            emitted_bytes: self.emitted_bytes,
            record_rejections: self.record_rejections.clone(),
        }
    }
}

fn default_base_source_path<R: JsonlFamilyRuntime>(
    _adapter: &(impl JsonlFamilyAdapter<Runtime = R> + ?Sized),
    certificate: &CertifiedSource,
) -> JsonlResult<PathBuf, JsonlRuntimeError<R>> {
    certificate
        .validate_contract()
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    // Parser revisions govern projection semantics, not source ownership. The
    // family still needs the prior source path so an unchanged source can be
    // selected and replaced under the current parser rather than rejected.
    let frontier = certificate.frontier().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload("JSONL base frontier is absent".to_owned())
    })?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base frontier kind changed".to_owned(),
        ));
    }
    let checkpoint =
        FamilyCheckpoint::decode_frontier_key::<JsonlRuntimeError<R>>(frontier.checkpoint())?;
    if checkpoint.physical.identity().source_descriptor_digest()
        != &certificate.observation().source().exact_descriptor_digest()
    {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base checkpoint source changed".to_owned(),
        ));
    }
    Ok(checkpoint.physical.identity().source_path().clone())
}

struct FamilyResident<E: JsonlFamilyError> {
    ownership_initialized: bool,
    owned_sources: HashMap<[u8; 32], SourceKey>,
    terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence<E>>,
    absent_sources: Vec<JsonlFamilyAbsentMember<E>>,
    opening_membership: Option<JsonlFamilyMembershipObservation<E>>,
    certified_inventory: Option<CertifiedSourceInventory>,
    opening_inventory: Option<JsonlFamilyInventory<E>>,
}

impl<E: JsonlFamilyError> Default for FamilyResident<E> {
    fn default() -> Self {
        Self {
            ownership_initialized: false,
            owned_sources: HashMap::new(),
            terminal_sources: HashMap::new(),
            absent_sources: Vec::new(),
            opening_membership: None,
            certified_inventory: None,
            opening_inventory: None,
        }
    }
}

#[derive(Debug)]
struct JsonlFamilyAbsentMember<E: JsonlFamilyError> {
    source_path: PathBuf,
    authority: Option<Arc<ProviderSourceRoot<E>>>,
    authority_path: PathBuf,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyAbsentMember<E> {
    fn clone(&self) -> Self {
        Self {
            source_path: self.source_path.clone(),
            authority: self.authority.as_ref().map(Arc::clone),
            authority_path: self.authority_path.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyAbsentMember<E> {
    fn from_path(opening: &JsonlFamilyInventory<E>, source_path: PathBuf) -> Option<Self> {
        if opening
            .authorities
            .iter()
            .any(|authority| source_path == authority.named_path())
        {
            return None;
        }
        let relative = opening.authorities.iter().find_map(|authority| {
            source_path
                .strip_prefix(authority.named_path())
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| (Arc::clone(authority), path.to_path_buf()))
        });
        Some(match relative {
            Some((authority, authority_path)) => Self {
                source_path,
                authority: Some(authority),
                authority_path,
            },
            None => Self {
                authority_path: PathBuf::new(),
                source_path,
                authority: None,
            },
        })
    }

    fn remains_absent(&self) -> JsonlResult<bool, E> {
        let opened = match &self.authority {
            Some(authority) => authority.open_path(&self.authority_path),
            None => open_provider_source_path::<E>(&self.source_path),
        };
        match opened {
            Ok(_) => Ok(false),
            Err(error) if error.is_not_found() => Ok(true),
            Err(error) => Err(error),
        }
    }
}

pub fn jsonl_family_driver<R: JsonlFamilyRuntime>(
    adapter: Arc<dyn JsonlFamilyAdapter<Runtime = R>>,
    root: PathBuf,
) -> super::JsonlRuntimeDriver<R> {
    let resident = Arc::new(Mutex::new(FamilyResident::<JsonlRuntimeError<R>>::default()));
    let scan_adapter = Arc::clone(&adapter);
    let scan_root = root.clone();
    let scan_resident = Arc::clone(&resident);
    let owns_adapter = Arc::clone(&adapter);
    let owns_resident = Arc::clone(&resident);
    let revalidation_resident = Arc::clone(&resident);
    let terminal_adapter = adapter;
    let terminal_root = root;
    let inventory_resident = Arc::clone(&resident);

    super::JsonlRuntimeDriver::<R>::new(
        move |sink| capture(&*scan_adapter, &scan_root, &scan_resident, sink),
        move |source| {
            owns_adapter.owns(source)
                && owns_resident.lock().is_ok_and(|resident| {
                    !resident.ownership_initialized
                        || resident
                            .owned_sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                })
        },
        move |target| revalidate_target(&revalidation_resident, target),
    )
    .with_parallel_leaf_workers()
    .with_fallible_complete_inventory_revalidation(move |expected| {
        match revalidate_complete_inventory(
            terminal_adapter.as_ref(),
            &terminal_root,
            &inventory_resident,
            expected,
        ) {
            Ok(revalidated) => Ok(revalidated),
            Err(error)
                if normalized_jsonl_error_kind(&error)
                    .unwrap_or_else(|| terminal_adapter.scan_error_kind(&error))
                    == SourceBackedRouteErrorKind::SourceChanged =>
            {
                Ok(false)
            }
            Err(error) => Err(route_scan(terminal_adapter.as_ref(), error)),
        }
    })
}

fn capture<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> SourceBackedRouteResult<()> {
    reset_terminal(resident)?;
    let opening = adapter
        .discover(root)
        .map_err(|error| route_discovery(adapter, error))?;
    let opening_membership = adapter
        .observe_terminal_membership(root, &opening)
        .map_err(|error| route_discovery(adapter, error))?;
    if opening.root_missing()
        && adapter.root_missing_mode() == JsonlFamilyRootMissingMode::Unavailable
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "provider JSONL root is unavailable",
        ));
    }
    if opening.leaves().is_empty() && !opening.rejected_leaves().is_empty() {
        let rejected_records =
            opening
                .rejected_leaves()
                .iter()
                .try_fold(0_u64, |total, leaf| {
                    total.checked_add(leaf.rejected_records).ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "provider JSONL rejected-record count overflow",
                        )
                    })
                })?;
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "direct JSONL route rejected {rejected_records} records across {} sources; \
                 all provider-native session identity leaves were rejected",
                opening.rejected_leaves().len(),
            ),
        ));
    }
    let bases = base_sources_for_root(adapter, &opening, root, sink)?;
    let mut selected_leaves = opening
        .leaves()
        .iter()
        .filter(|leaf| {
            adapter.base_scope() == JsonlFamilyBaseScope::ProviderFamily
                || !sink.source_owned_by_other_route(leaf.source())
        })
        .cloned()
        .collect::<Vec<_>>();
    adapter
        .order_leaf_scans(&mut selected_leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let mut owned_sources = HashMap::with_capacity(bases.len() + selected_leaves.len());
    for source in bases
        .iter()
        .map(|base| base.observation().source())
        .chain(selected_leaves.iter().map(JsonlFamilyLeaf::source))
    {
        let digest = source.exact_descriptor_digest();
        if owned_sources
            .insert(digest, source.clone())
            .is_some_and(|previous| !previous.exact_descriptor_eq(source))
        {
            return Err(route_invalid(
                "JSONL route source descriptor digest collision",
            ));
        }
    }
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let base_event_lookup = sink.base_event_lookup();
    let mut scan_selected_leaves = Vec::with_capacity(selected_leaves.len());
    let mut retained_terminal_sources = HashMap::new();
    for leaf in &selected_leaves {
        let Some(base) = base_for_leaf(&bases_by_descriptor, leaf) else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        let Ok(observation) =
            source_observation::<JsonlRuntimeError<R>>(leaf.source(), leaf.observation())
        else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if observation != *base.observation() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        let Ok(checkpoint) = decode_checkpoint(adapter, leaf, base) else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if !checkpoint.physical.terminal() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        let terminal_proof = JsonlFamilyTerminalProof::unchanged(adapter, leaf, base, &checkpoint)
            .map_err(|error| route_scan(adapter, error))?;
        sink.retain_source(base.clone()).map_err(route_internal)?;
        sink.report_completed_bytes(base.counts().certified_bytes)
            .map_err(route_internal)?;
        retained_terminal_sources.insert(
            leaf.source().exact_descriptor_digest(),
            TerminalSourceEvidence {
                certificate: base.clone(),
                terminal_proof,
                emitted_bytes: 0,
                record_rejections: SourceBackedRecordRejectionDrafts::default(),
            },
        );
    }
    let terminal_sources = scan_leaves(
        adapter,
        &scan_selected_leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let mut terminal_sources = terminal_sources?;
    finish_leaf_scans?;
    for (digest, evidence) in retained_terminal_sources {
        if terminal_sources.insert(digest, evidence).is_some() {
            return Err(route_invalid("duplicate JSONL terminal source evidence"));
        }
    }

    let selected_sources = selected_leaves
        .iter()
        .map(|leaf| leaf.source().clone())
        .collect::<Vec<_>>();
    let inventory = opening
        .certify_selected_against(&opening, selected_sources)
        .map_err(route_invalid)?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_internal)?;
    let mut absent_sources = Vec::new();
    for base in &bases {
        if !inventory.contains(base.observation().source()) {
            if let Some(absent) = JsonlFamilyAbsentMember::from_path(
                &opening,
                adapter
                    .base_source_path(base)
                    .map_err(|error| route_scan(adapter, error))?,
            ) {
                absent_sources.push(absent);
            }
            let deletion = CertifiedSourceDeletion::from_inventory(
                base.observation().source().clone(),
                &inventory,
            )
            .map_err(route_invalid)?;
            sink.delete_source(deletion, inventory.clone())
                .map_err(route_internal)?;
        }
    }
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.ownership_initialized = true;
    resident.owned_sources = owned_sources;
    resident.terminal_sources = terminal_sources;
    resident.absent_sources = absent_sources;
    resident.opening_membership = Some(opening_membership);
    resident.certified_inventory = Some(inventory);
    resident.opening_inventory = Some(opening);
    Ok(())
}

fn bases_by_descriptor(
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<HashMap<[u8; 32], &CertifiedSource>> {
    let mut by_descriptor = HashMap::with_capacity(bases.len());
    for base in bases {
        let source = base.observation().source();
        let digest = source.exact_descriptor_digest();
        if let Some(previous) = by_descriptor.insert(digest, base) {
            if !previous.observation().source().exact_descriptor_eq(source) {
                return Err(route_invalid(
                    "JSONL base source descriptor digest collision",
                ));
            }
            return Err(route_invalid("duplicate JSONL base source descriptor"));
        }
    }
    Ok(by_descriptor)
}

#[cfg(test)]
#[path = "route/tests.rs"]
mod tests;
