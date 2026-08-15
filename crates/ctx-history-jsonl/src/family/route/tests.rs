use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use super::super::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_jsonl_prefix_hash_hook,
    JsonlReader as RuntimeJsonlReader,
};
use super::*;
use ctx_history_capture_model::AttemptHistoryProgress;
use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
    CaptureRevalidationTarget, CaptureRouteRef, CaptureSourceAggregateRef, CoreMaterialization,
    CorePreparationFailureKind, CorePreparationPort, ImmutableCaptureSnapshot, PresentCaptureRoute,
    SourceBackedGenerationSink as RuntimeSourceBackedGenerationSink,
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections,
    SourceBackedRevalidationTarget, SourceBackedRouteResources, VerifiedCapture,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSourceAppend, CertifiedSourceDeletion, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor,
};
use ctx_history_source_io::{SourceIoError, MAX_PROVIDER_JSONL_LINE_BYTES};

#[path = "tests/behavior.rs"]
mod behavior;
#[path = "tests/checkpoint_lifecycle.rs"]
mod checkpoint_lifecycle;

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

fn test_contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

type CaptureError = SourceIoError;
type Result<T> = std::result::Result<T, CaptureError>;
type OpenedProviderSourceFile = super::super::OpenedProviderSourceFile<CaptureError>;
type ProviderSourceRoot = super::super::ProviderSourceRoot<CaptureError>;
type JsonlReader = RuntimeJsonlReader<CaptureError>;
type JsonlFamilyLeaf = super::JsonlFamilyLeaf<CaptureError>;
type JsonlFamilyInventory = super::JsonlFamilyInventory<CaptureError>;
type JsonlFamilyMembershipObservation = super::JsonlFamilyMembershipObservation<CaptureError>;
type JsonlFamilyTerminalProof = super::JsonlFamilyTerminalProof<CaptureError>;
type JsonlFamilyOptimizedLeafOutcome = super::JsonlFamilyOptimizedLeafOutcome<CaptureError>;
type JsonlFamilyWorkerContext = super::JsonlFamilyWorkerContext<TestJsonlRuntime>;
type JsonlFamilyExecutionIo = super::JsonlFamilyExecutionIo<TestJsonlRuntime>;
type JsonlFamilyAdapterObject = dyn JsonlFamilyAdapter<Runtime = TestJsonlRuntime>;
type JsonlFamilyProjectorObject = dyn JsonlFamilyProjector<Runtime = TestJsonlRuntime>;
type JsonlFamilySemanticExecutorObject =
    dyn JsonlFamilySemanticExecutor<Runtime = TestJsonlRuntime>;
type FamilyResident = super::FamilyResident<CaptureError>;
type TerminalSourceEvidence = super::TerminalSourceEvidence<CaptureError>;
type JsonlFamilyAbsentMember = super::JsonlFamilyAbsentMember<CaptureError>;
type IndexBaseEventLookup = TestBaseEventLookup;
type IndexCaptureLifecycle = TestLifecycle;
type SourceBackedGenerationSink<'writer> =
    RuntimeSourceBackedGenerationSink<'writer, TestLifecycle>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct JsonlFamilyAdmissionActivity {
    selected_leaves: usize,
    bases: usize,
    retained_terminal_sources: usize,
    checkpoint_rejections: usize,
}

thread_local! {
    static JSONL_FAMILY_ADMISSION_ACTIVITY: std::cell::Cell<JsonlFamilyAdmissionActivity> =
        const { std::cell::Cell::new(JsonlFamilyAdmissionActivity {
            selected_leaves: 0,
            bases: 0,
            retained_terminal_sources: 0,
            checkpoint_rejections: 0,
        }) };
}

fn jsonl_family_admission_activity() -> JsonlFamilyAdmissionActivity {
    JSONL_FAMILY_ADMISSION_ACTIVITY.get()
}

pub(super) fn begin_admission(selected_leaves: usize, bases: usize) {
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(JsonlFamilyAdmissionActivity {
        selected_leaves,
        bases,
        ..JsonlFamilyAdmissionActivity::default()
    });
}

pub(super) fn record_checkpoint_rejection() {
    let mut activity = JSONL_FAMILY_ADMISSION_ACTIVITY.get();
    activity.checkpoint_rejections += 1;
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(activity);
}

pub(super) fn record_retained_sources(retained_terminal_sources: usize) {
    let mut activity = JSONL_FAMILY_ADMISSION_ACTIVITY.get();
    activity.retained_terminal_sources = retained_terminal_sources;
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(activity);
}

#[derive(Default)]
struct TestWorkerServices {
    certified_repositories: HashSet<PathBuf>,
    full_certification_probes: usize,
    event_time_entries: usize,
}

impl TestWorkerServices {
    fn begin_source(&mut self) {
        self.event_time_entries = 0;
    }

    fn attribute(&mut self, repository: &Path) -> bool {
        if self.certified_repositories.insert(repository.to_path_buf()) {
            self.full_certification_probes = self.full_certification_probes.saturating_add(1);
        }
        self.event_time_entries = self.event_time_entries.saturating_add(1);
        true
    }

    fn full_certification_probe_count(&self) -> usize {
        self.full_certification_probes
    }

    fn event_time_cache_len(&self) -> usize {
        self.event_time_entries
    }
}

struct TestJsonlRuntime;

impl JsonlFamilyRuntime for TestJsonlRuntime {
    type Error = CaptureError;
    type Lifecycle = TestLifecycle;
    type WorkerServices = TestWorkerServices;
    type RouteControl = ();

    fn begin_worker_leaf(services: &mut Self::WorkerServices) {
        services.begin_source();
    }
}

#[derive(Clone, Default)]
struct TestBaseEventLookup {
    events: HashSet<uuid::Uuid>,
}

impl BaseEventLookup for TestBaseEventLookup {
    type Error = CaptureError;

    fn contains(&self, event_id: uuid::Uuid) -> Result<bool> {
        Ok(self.events.contains(&event_id))
    }
}

#[derive(Clone, Default)]
struct TestPreparation;

struct TestPreparedRecord {
    record: CoreRecord,
    encoded_bytes: usize,
}

impl CorePreparationPort for TestPreparation {
    type Prepared = TestPreparedRecord;
    type Draft = CoreRecord;
    type Failure = CaptureError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared> {
        let encoded_bytes = record
            .encode_stored()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
            .len();
        Ok(TestPreparedRecord {
            record,
            encoded_bytes,
        })
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft> {
        record
            .validate_contract()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>> {
        let encoded_bytes = draft
            .encode_stored()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
            .len();
        if encoded_bytes > maximum_encoded_bytes {
            return Ok(CoreMaterialization::CapacityExceeded(Box::new(draft)));
        }
        Ok(CoreMaterialization::Prepared(TestPreparedRecord {
            record: draft,
            encoded_bytes,
        }))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.record.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_bytes
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::InvalidSource
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TestSnapshot {
    sources: Vec<CertifiedSource>,
    route_identity: Option<SourceRouteIdentity>,
    route_sources: Vec<SourceKey>,
    records: Vec<CoreRecord>,
}

impl ImmutableCaptureSnapshot for TestSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &self.sources
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        std::iter::empty()
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
            .into_iter()
    }

    fn source_route(&self, route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .filter(|identity| *identity == route_identity)
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
    }
}

#[derive(Debug)]
struct IndexCaptureCommitReceipt {
    generation_id: String,
    manifest: TestSnapshot,
}

impl IndexCaptureCommitReceipt {
    fn new(receipt: CaptureCommitReceipt<TestSnapshot>) -> Self {
        let (generation_id, _, _, _, _, manifest) = receipt.into_parts();
        Self {
            generation_id,
            manifest,
        }
    }

    fn manifest(&self) -> &TestSnapshot {
        &self.manifest
    }
}

fn test_generations() -> &'static Mutex<HashMap<PathBuf, TestSnapshot>> {
    static GENERATIONS: OnceLock<Mutex<HashMap<PathBuf, TestSnapshot>>> = OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct TestLifecycle {
    root: PathBuf,
    base: Option<TestSnapshot>,
    current_source: Option<SourceKey>,
    records: Vec<CoreRecord>,
    certified_sources: Vec<CertifiedSource>,
    activity: TestLifecycleActivity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TestLifecycleActivity {
    begin_source_replacements: usize,
    begin_source_appends: usize,
    retained_sources: usize,
}

impl TestLifecycle {
    fn snapshot(&self) -> TestSnapshot {
        let mut sources = self.certified_sources.clone();
        sources.sort_by(|left, right| {
            left.observation()
                .source()
                .cmp(right.observation().source())
        });
        TestSnapshot {
            route_identity: Some(test_route_identity()),
            route_sources: sources
                .iter()
                .map(|source| source.observation().source().clone())
                .collect(),
            sources,
            records: self.records.clone(),
        }
    }

    fn base_event_identity_lookup(&self) -> TestBaseEventLookup {
        self.base_event_lookup()
    }

    fn activity(&self) -> TestLifecycleActivity {
        self.activity
    }

    fn commit_receipt(self) -> CaptureCommitReceipt<TestSnapshot> {
        let root = self.root.clone();
        let snapshot = self.snapshot();
        let indexed_documents = snapshot
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum();
        let certified_source_bytes = snapshot
            .sources
            .iter()
            .map(|source| source.counts().certified_bytes)
            .sum();
        let mut generations = test_generations().lock().unwrap();
        let next_opstamp = if self.base.as_ref() == Some(&snapshot) {
            1
        } else {
            generations.get(&root).map_or(1, |_| 2)
        };
        let generation_id = format!("test-generation-{next_opstamp}");
        generations.insert(root, snapshot.clone());
        CaptureCommitReceipt::new(
            generation_id,
            next_opstamp,
            indexed_documents,
            snapshot.sources.len(),
            certified_source_bytes,
            snapshot,
        )
    }
}

impl CaptureLifecycleSink for TestLifecycle {
    type Error = CaptureError;
    type OpenOptions = ();
    type BaseLookup = TestBaseEventLookup;
    type Preparation = TestPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = TestSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = TestSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        CaptureError::SystemInvariant(detail)
    }

    fn open(root: &Path, _options: Self::OpenOptions) -> Result<CaptureLifecycleOpenOutcome<Self>> {
        let base = test_generations().lock().unwrap().get(root).cloned();
        let records = base
            .as_ref()
            .map(|snapshot| snapshot.records.clone())
            .unwrap_or_default();
        Ok(CaptureLifecycleOpenOutcome::Ready(Self {
            root: root.to_path_buf(),
            base,
            current_source: None,
            records,
            certified_sources: Vec::new(),
            activity: TestLifecycleActivity::default(),
        }))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        self.base.clone()
    }

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.base
            .as_ref()?
            .sources
            .iter()
            .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        self.base_source(source).cloned()
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        TestBaseEventLookup {
            events: self
                .base
                .iter()
                .flat_map(|snapshot| {
                    snapshot
                        .records
                        .iter()
                        .map(|record| record.event_id.as_uuid())
                })
                .collect(),
        }
    }

    fn core_preparation(&self) -> Self::Preparation {
        TestPreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        Ok(())
    }

    fn begin_route_stage(&mut self, _route_identity: SourceRouteIdentity) -> Result<()> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<()> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        mut visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> std::result::Result<(), E>,
    ) -> Result<std::result::Result<(), E>> {
        for source in &self.certified_sources {
            if let Err(error) = visit(CaptureRevalidationTarget::Source(source)) {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn finish_route_stage(&mut self, _route_identity: &SourceRouteIdentity) -> Result<()> {
        Ok(())
    }

    fn rollback_route_stage(&mut self, _route_identity: &SourceRouteIdentity) -> Result<()> {
        self.current_source = None;
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<()> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<()> {
        self.activity.begin_source_replacements += 1;
        self.records
            .retain(|record| !record.source.exact_descriptor_eq(&source));
        self.current_source = Some(source);
        Ok(())
    }

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource> {
        self.activity.begin_source_appends += 1;
        self.current_source = Some(source.clone());
        self.base_source(&source)
            .ok_or(CaptureError::SystemInvariant("append source has no base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource> {
        self.begin_source_append(base.observation().source().clone())
    }

    fn add_prepared(&mut self, prepared: TestPreparedRecord) -> Result<()> {
        self.records.push(prepared.record);
        Ok(())
    }

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        self.certified_sources.push(certificate);
        self.current_source = None;
        Ok(())
    }

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<()> {
        self.certified_sources.push(append.into_current());
        self.current_source = None;
        Ok(())
    }

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        self.activity.retained_sources += 1;
        self.certified_sources.push(certificate);
        Ok(())
    }

    fn certify_complete_inventory(&mut self, _inventory: CertifiedSourceInventory) -> Result<()> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        self.records
            .retain(|record| !record.source.exact_descriptor_eq(deletion.source()));
        Ok(())
    }

    fn carry_failed_route(&mut self, _route_identity: &SourceRouteIdentity) -> Result<bool> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<()> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<()> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<TestSnapshot>>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self.commit_receipt())
    }

    fn commit_with_metadata<F, I, M>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<CaptureCommitOutcome<TestSnapshot, ()>>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(CapturePublicationContext<'a, Self::Snapshot<'a>>) -> Result<Vec<u8>>,
    {
        let snapshot = self.snapshot();
        metadata_factory(CapturePublicationContext::new("test-generation", snapshot))?;
        Ok(CaptureCommitOutcome::new(
            self.commit_receipt(),
            CapturePublicationDisposition::Published,
            VerifiedCapture::new(()),
        ))
    }
}

macro_rules! capture_test_generation {
    ($adapter:expr, $root:expr, $index_root:expr, $workers:expr, $capture:expr) => {{
        let resident = Mutex::new(FamilyResident::default());
        let mut writer = match IndexCaptureLifecycle::open($index_root, ()).unwrap() {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
                panic!("test lifecycle unexpectedly requires recovery")
            }
        };
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let result = {
            let mut applied_removals = Vec::new();
            let mut sink = SourceBackedGenerationSink::new(
                &mut writer,
                &mut owners,
                &mut complete_inventories,
                &mut applied_removals,
                0,
                test_route_identity(),
                None,
                SourceBackedRouteResources::production($workers),
                &mut logical_source_failures,
                &mut record_rejections,
                None,
                None,
                None,
            );
            with_family_scanner_workers($workers, || $capture(&resident, &mut sink))
        };
        (writer, resident, result)
    }};
}

fn capture_test_generation_without_commit(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> TestLifecycle {
    let (writer, _resident, result) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink)
        });
    result.unwrap();
    writer
}

struct TestAdapter;

const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";
const PROGRESS_TEST_RECORDS: &[u8] =
    b"{\"message\":\"one\"}\n{\"message\":\"two\"}\n{\"tool_call\":\"three\"}\n";

impl JsonlFamilyAdapter for TestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-witness-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        if !root.exists() {
            return JsonlFamilyInventory::missing(self.provider(), root);
        }
        let authority = Arc::new(ProviderSourceRoot::open(root)?);
        let mut leaves = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let name = entry.file_name();
            let source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "terminal-witness-file",
                    TypedKey::bytes(name.as_encoded_bytes().to_vec())
                        .map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?,
            )
            .map_err(test_contract_error)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                PathBuf::from(&name),
                TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(test_contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal witness tests never project",
        ))
    }
}

macro_rules! impl_standard_jsonl_test_adapter {
    (
        $adapter:ty,
        $parser_revision:literal,
        $append_mode:expr,
        |$this:ident, $leaf:ident, $source_file:ident, $imported_at:ident| $projector:block
        $(, |$framing_adapter:ident| $record_framing:expr)?
    ) => {
        impl JsonlFamilyAdapter for $adapter {
            type Runtime = TestJsonlRuntime;

            fn provider(&self) -> CaptureProvider {
                CaptureProvider::Pi
            }

            fn source_format(&self) -> &'static str {
                TEST_SOURCE_FORMAT
            }

            fn schema_variant(&self) -> &'static str {
                TEST_SCHEMA
            }

            fn parser_revision(&self) -> &'static str {
                $parser_revision
            }

            fn append_mode(&self) -> JsonlFamilyAppendMode {
                $append_mode
            }

            $(
                fn record_framing(&self) -> JsonlRecordFraming {
                    let $framing_adapter = self;
                    $record_framing
                }
            )?

            fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
                TestAdapter.discover(root)
            }

            fn projector(
                &self,
                leaf: &JsonlFamilyLeaf,
                source_file: Arc<OpenedProviderSourceFile>,
                imported_at: DateTime<Utc>,
            ) -> Result<Box<JsonlFamilyProjectorObject>> {
                let $this = self;
                let $leaf = leaf;
                let $source_file = source_file;
                let $imported_at = imported_at;
                $projector
            }
        }
    };
}

#[cfg(unix)]
struct TerminalLeafSwapTestAdapter {
    selected: PathBuf,
    outside: PathBuf,
    enabled: AtomicBool,
    swapped: AtomicBool,
}

#[cfg(unix)]
impl JsonlFamilyAdapter for TerminalLeafSwapTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        TestAdapter.provider()
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-leaf-swap-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        if self.enabled.load(Ordering::SeqCst) && !self.swapped.swap(true, Ordering::SeqCst) {
            fs::remove_file(&self.selected)?;
            std::os::unix::fs::symlink(&self.outside, &self.selected)?;
        }
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal leaf swap tests never project",
        ))
    }
}

fn expected_state(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
) -> (FamilyResident, CertifiedSourceInventory) {
    let observed = adapter.discover(root).unwrap();
    let opening_membership = adapter
        .observe_terminal_membership(root, &observed)
        .unwrap();
    let inventory = observed.certify_against(&observed).unwrap();
    let terminal_sources = observed
        .leaves()
        .iter()
        .map(|leaf| {
            let opened = leaf.open_verified().unwrap();
            let mut reader =
                JsonlReader::open(physical_identity(adapter, leaf), opened, None, None).unwrap();
            while reader
                .visit_page(&mut |_record| -> Result<()> { Ok(()) })
                .unwrap()
                .is_some()
            {}
            let checkpoint = reader.outcome().unwrap().checkpoint().clone();
            let observation = scanner::source_observation::<CaptureError>(
                leaf.source(),
                checkpoint.source_observation(),
            )
            .unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                adapter.parser_revision(),
                *checkpoint.complete_prefix_sha256(),
                ScannedSourceCounts::default(),
            )
            .unwrap();
            let terminal_proof = JsonlFamilyTerminalProof::frozen_shared_prefix(
                adapter,
                leaf,
                &certificate,
                checkpoint.complete_prefix_end(),
                *checkpoint.complete_prefix_sha256(),
            )
            .unwrap();
            (
                leaf.source().exact_descriptor_digest(),
                TerminalSourceEvidence {
                    certificate,
                    terminal_proof,
                    emitted_bytes: 0,
                    exact_scan_bytes: None,
                    record_rejections: SourceBackedRecordRejectionDrafts::default(),
                },
            )
        })
        .collect();
    let owned_sources = observed
        .leaves()
        .iter()
        .map(|leaf| {
            (
                leaf.source().exact_descriptor_digest(),
                leaf.source().clone(),
            )
        })
        .collect();
    (
        FamilyResident {
            ownership_initialized: true,
            owned_sources,
            terminal_sources,
            absent_sources: Vec::new(),
            opening_membership: Some(opening_membership),
            certified_inventory: Some(inventory.clone()),
            opening_inventory: Some(observed),
        },
        inventory,
    )
}

struct FrozenMultiRootTestAdapter {
    roots: Vec<PathBuf>,
}

impl JsonlFamilyAdapter for FrozenMultiRootTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "frozen-multi-root-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let mut authorities = Vec::new();
        let mut leaves = Vec::new();
        for source_root in &self.roots {
            let authority = Arc::new(ProviderSourceRoot::open(source_root)?);
            for entry in fs::read_dir(source_root)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                let name = entry.file_name();
                let source = SourceKey::derive(
                    self.provider().as_str(),
                    TEST_SOURCE_FORMAT,
                    TEST_SCHEMA,
                    1,
                    SourceAnchor::provider_native(
                        "frozen-multi-root-file",
                        TypedKey::bytes(path.as_os_str().as_encoded_bytes().to_vec())
                            .map_err(test_contract_error)?,
                    )
                    .map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?;
                leaves.push(JsonlFamilyLeaf::observe(
                    source,
                    path,
                    Arc::clone(&authority),
                    PathBuf::from(&name),
                    TypedKey::bytes(name.as_encoded_bytes().to_vec())
                        .map_err(test_contract_error)?,
                )?);
            }
            authorities.push(authority);
        }
        authorities.reverse();
        JsonlFamilyInventory::present_multi(self.provider(), root, authorities, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "frozen inventory tests never project",
        ))
    }
}

struct TerminalRootSwapTestAdapter {
    root: PathBuf,
    discoveries: AtomicUsize,
}

impl JsonlFamilyAdapter for TerminalRootSwapTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-root-swap-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, selection_root: &Path) -> Result<JsonlFamilyInventory> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        FrozenMultiRootTestAdapter {
            roots: vec![self.root.clone()],
        }
        .discover(selection_root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal root swap tests never project",
        ))
    }
}

fn expected_source(resident: &FamilyResident) -> CertifiedSource {
    resident
        .terminal_sources
        .values()
        .next()
        .unwrap()
        .certificate
        .clone()
}

struct ParallelTestAdapter;

struct ParallelTestProjector;

impl JsonlFamilyProjector for ParallelTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    ParallelTestAdapter,
    "parallel-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |_adapter, _leaf, _source_file, _imported_at| { Ok(Box::new(ParallelTestProjector)) }
);

struct ReplacementParallelTestAdapter;

impl_standard_jsonl_test_adapter!(
    ReplacementParallelTestAdapter,
    "replacement-parallel-test-parser-v1",
    JsonlFamilyAppendMode::Replacement,
    |_adapter, _leaf, _source_file, _imported_at| { Ok(Box::new(ParallelTestProjector)) }
);

struct PhasedTestAdapter {
    completed_first_phase: Arc<AtomicUsize>,
    second_phase_started_early: Arc<AtomicBool>,
}

struct PhasedTestProjector {
    phase: usize,
    completed_first_phase: Arc<AtomicUsize>,
    second_phase_started_early: Arc<AtomicBool>,
}

impl JsonlFamilyProjector for PhasedTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.phase == 1 && self.completed_first_phase.load(Ordering::SeqCst) != 4 {
            self.second_phase_started_early
                .store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.phase == 0 {
            self.completed_first_phase.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl JsonlFamilyAdapter for PhasedTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "phased-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by_key(|leaf| self.leaf_scan_phase(leaf).unwrap_or(usize::MAX));
        Ok(())
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        Ok(usize::from(
            leaf.source_path()
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("second-")),
        ))
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(PhasedTestProjector {
            phase: self.leaf_scan_phase(leaf)?,
            completed_first_phase: Arc::clone(&self.completed_first_phase),
            second_phase_started_early: Arc::clone(&self.second_phase_started_early),
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SchedulerLeafState {
    partition: u64,
    phase: usize,
    ordinal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchedulerStateEvent {
    Begin(u64),
    Finish(u64),
    Project {
        leaf: SchedulerLeafState,
        full_probes_before: usize,
        full_probes_after: usize,
        event_time_entries_before: usize,
        event_time_entries_after: usize,
    },
}

struct SchedulerStateTestAdapter {
    repository: PathBuf,
    attributed_partitions: Vec<u64>,
    failing_leaf: Option<SchedulerLeafState>,
    parallel_frontier: Option<(u64, usize, Arc<std::sync::Barrier>)>,
    events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

struct UnpartitionedSchedulerStateTestAdapter(SchedulerStateTestAdapter);

struct SchedulerStateTestProjector {
    leaf: SchedulerLeafState,
    repository: PathBuf,
    attribute_repository: bool,
    fail: bool,
    parallel_frontier: Option<Arc<std::sync::Barrier>>,
    events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

fn scheduler_leaf_state(leaf: &JsonlFamilyLeaf) -> Result<SchedulerLeafState> {
    let name = leaf
        .source_path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.strip_suffix(".jsonl"))
        .ok_or_else(|| {
            CaptureError::InvalidPayload("scheduler test leaf name is malformed".to_owned())
        })?;
    let fields = name.split('-').collect::<Vec<_>>();
    if fields.len() != 6 || fields[0] != "partition" || fields[2] != "phase" || fields[4] != "leaf"
    {
        return Err(CaptureError::InvalidPayload(
            "scheduler test leaf name is malformed".to_owned(),
        ));
    }
    let partition = fields[1].parse::<u64>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test partition is malformed".to_owned())
    })?;
    let phase = fields[3].parse::<usize>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test phase is malformed".to_owned())
    })?;
    let ordinal = fields[5].parse::<usize>().map_err(|_| {
        CaptureError::InvalidPayload("scheduler test ordinal is malformed".to_owned())
    })?;
    Ok(SchedulerLeafState {
        partition,
        phase,
        ordinal,
    })
}

impl JsonlFamilyProjector for SchedulerStateTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.fail {
            return Err(CaptureError::InvalidPayload(
                "scheduler test requested scan failure".to_owned(),
            ));
        }
        if let Some(barrier) = &self.parallel_frontier {
            barrier.wait();
        }
        let full_probes_before = worker.services().full_certification_probe_count();
        let event_time_entries_before = worker.services().event_time_cache_len();
        if self.attribute_repository && !worker.services().attribute(&self.repository) {
            return Err(CaptureError::InvalidPayload(
                "scheduler test repository attribution did not bind".to_owned(),
            ));
        }
        let full_probes_after = worker.services().full_certification_probe_count();
        let event_time_entries_after = worker.services().event_time_cache_len();
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Project {
                leaf: self.leaf,
                full_probes_before,
                full_probes_after,
                event_time_entries_before,
                event_time_entries_after,
            });
        Ok(())
    }
}

impl JsonlFamilyAdapter for SchedulerStateTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "scheduler-state-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        leaves.sort_by_key(|leaf| {
            scheduler_leaf_state(leaf).unwrap_or(SchedulerLeafState {
                partition: u64::MAX,
                phase: usize::MAX,
                ordinal: usize::MAX,
            })
        });
        Ok(())
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        Ok(scheduler_leaf_state(leaf)?.phase)
    }

    fn leaf_scan_partition(&self, leaf: &JsonlFamilyLeaf) -> Result<Option<u64>> {
        Ok(Some(scheduler_leaf_state(leaf)?.partition))
    }

    fn begin_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Begin(partition));
        Ok(())
    }

    fn finish_leaf_scan_partition(&self, partition: u64) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| CaptureError::SystemInvariant("scheduler test event log was poisoned"))?
            .push(SchedulerStateEvent::Finish(partition));
        Ok(())
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        let leaf = scheduler_leaf_state(leaf)?;
        let parallel_frontier = self
            .parallel_frontier
            .as_ref()
            .filter(|(partition, phase, _)| *partition == leaf.partition && *phase == leaf.phase)
            .map(|(_, _, barrier)| Arc::clone(barrier));
        Ok(Box::new(SchedulerStateTestProjector {
            leaf,
            repository: self.repository.clone(),
            attribute_repository: self.attributed_partitions.contains(&leaf.partition),
            fail: self.failing_leaf == Some(leaf),
            parallel_frontier,
            events: Arc::clone(&self.events),
        }))
    }
}

impl JsonlFamilyAdapter for UnpartitionedSchedulerStateTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        self.0.provider()
    }

    fn source_format(&self) -> &'static str {
        self.0.source_format()
    }

    fn schema_variant(&self) -> &'static str {
        self.0.schema_variant()
    }

    fn parser_revision(&self) -> &'static str {
        self.0.parser_revision()
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        self.0.append_mode()
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        self.0.discover(root)
    }

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        self.0.order_leaf_scans(leaves)
    }

    fn leaf_scan_phase(&self, leaf: &JsonlFamilyLeaf) -> Result<usize> {
        self.0.leaf_scan_phase(leaf)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        self.0.projector(leaf, source_file, imported_at)
    }
}

struct IdentityRevisionTestAdapter {
    parser_revision: &'static str,
    revision: &'static str,
    expected_mode: JsonlFamilyProjectionMode,
}

impl JsonlFamilyAdapter for IdentityRevisionTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        self.parser_revision
    }

    fn event_identity_revision(&self) -> &'static str {
        self.revision
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(ParallelTestProjector))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        if checkpoint.is_some()
            || mode != self.expected_mode
            || base_event_lookup.is_some() != (mode != JsonlFamilyProjectionMode::Cold)
        {
            return Err(CaptureError::InvalidPayload(
                "identity revision test received inconsistent projection context".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticLifecycleBehavior {
    RetryAppend,
    Overclassify,
    StopBeforeTerminal,
}

#[derive(Debug, Default)]
struct SemanticLifecycleObservations {
    constructed_modes: Vec<JsonlFamilyProjectionMode>,
    preflight_modes: Vec<JsonlFamilyProjectionMode>,
    page_modes: Vec<JsonlFamilyProjectionMode>,
    finished_modes: Vec<JsonlFamilyProjectionMode>,
}

struct SemanticLifecycleTestAdapter {
    behavior: SemanticLifecycleBehavior,
    observations: Arc<Mutex<SemanticLifecycleObservations>>,
}

struct SemanticLifecycleTestExecutor {
    behavior: SemanticLifecycleBehavior,
    mode: JsonlFamilyProjectionMode,
    observations: Arc<Mutex<SemanticLifecycleObservations>>,
    consumed: u64,
}

impl JsonlFamilySemanticExecutor for SemanticLifecycleTestExecutor {
    type Runtime = TestJsonlRuntime;

    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
        self.observations
            .lock()
            .unwrap()
            .preflight_modes
            .push(self.mode);
        if self.behavior == SemanticLifecycleBehavior::RetryAppend
            && self.mode == JsonlFamilyProjectionMode::CertifiedAppend
        {
            return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
        }
        while let Some(record) = input.next_record()? {
            let _ = input.record_bytes(record)?;
        }
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        _worker: &mut JsonlFamilyWorkerContext,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        self.observations.lock().unwrap().page_modes.push(self.mode);
        if self.behavior == SemanticLifecycleBehavior::StopBeforeTerminal {
            return Ok(None);
        }
        let Some(record) = input.next_record()? else {
            return Ok(None);
        };
        let _ = input.record_bytes(record)?;
        self.consumed = self.consumed.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("semantic lifecycle test count overflowed".to_owned())
        })?;
        Ok(Some(JsonlFamilySemanticPage::new(Vec::new())))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        self.observations
            .lock()
            .unwrap()
            .finished_modes
            .push(self.mode);
        let represented = if self.behavior == SemanticLifecycleBehavior::Overclassify {
            self.consumed.saturating_add(1)
        } else {
            0
        };
        Ok(JsonlFamilySemanticSummary::with_logical_counts(
            represented,
            0,
            self.consumed,
            0,
            None,
        ))
    }
}

impl JsonlFamilyAdapter for SemanticLifecycleTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "semantic-lifecycle-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "semantic lifecycle tests require the semantic executor",
        ))
    }

    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<JsonlFamilySemanticExecutorObject>>> {
        self.observations
            .lock()
            .unwrap()
            .constructed_modes
            .push(mode);
        Ok(Some(Box::new(SemanticLifecycleTestExecutor {
            behavior: self.behavior,
            mode,
            observations: Arc::clone(&self.observations),
            consumed: 0,
        })))
    }
}

struct EmissionTestAdapter {
    project_fanout: usize,
    finish_fanout: usize,
    admitted: Option<Arc<AtomicUsize>>,
    observed_before_65: Option<Arc<AtomicUsize>>,
}

struct EmissionTestProjector {
    source: SourceKey,
    project_fanout: usize,
    finish_fanout: usize,
    admitted: Option<Arc<AtomicUsize>>,
    observed_before_65: Option<Arc<AtomicUsize>>,
}

impl EmissionTestAdapter {
    fn ordinary() -> Self {
        Self {
            project_fanout: 1,
            finish_fanout: 0,
            admitted: None,
            observed_before_65: None,
        }
    }
}

fn emission_test_record(source: &SourceKey, ordinal: u64) -> Result<CoreRecord> {
    emission_test_typed_record(source, ordinal, "message")
}

fn emission_test_typed_record(
    source: &SourceKey,
    ordinal: u64,
    event_type: &'static str,
) -> Result<CoreRecord> {
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("session").map_err(test_contract_error)?,
    )
    .map_err(test_contract_error)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .map_err(test_contract_error)?;
    let native_item_key = NativeItemKey::native_id(event_type, TypedKey::U64(ordinal))
        .map_err(test_contract_error)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: event_type,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(test_contract_error)?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        ordinal,
        event_type,
        "primary",
        true,
        "jsonl-emission-test-v1",
        "bounded",
    )
    .map_err(test_contract_error)?;
    projected.provider_session_id = Some("session".to_owned());
    projected.native_event_id = Some(TypedKey::U64(ordinal));
    projected.occurred_at_unix_ms = Some(ordinal as i64);
    projected.role = Some("user".to_owned());
    Ok(projected)
}

impl JsonlFamilyProjector for EmissionTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let base = record
            .evidence()
            .physical_ordinal()
            .checked_mul(1_000)
            .ok_or(CaptureError::SystemInvariant(
                "emission-test ordinal overflowed",
            ))?;
        self.emit_fanout(base, self.project_fanout, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.emit_fanout(1_000_000, self.finish_fanout, emit)
    }
}

impl EmissionTestProjector {
    fn emit_fanout(
        &self,
        base: u64,
        count: usize,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        for index in 0..count {
            if index == 64 {
                if let (Some(admitted), Some(observed)) =
                    (self.admitted.as_ref(), self.observed_before_65.as_ref())
                {
                    observed.store(admitted.load(Ordering::SeqCst), Ordering::SeqCst);
                }
            }
            let ordinal = base
                .checked_add(index as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "emission-test fanout overflowed",
                ))?;
            emit(emission_test_record(&self.source, ordinal)?)?;
        }
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    EmissionTestAdapter,
    "emission-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |adapter, leaf, _source_file, _imported_at| {
        Ok(Box::new(EmissionTestProjector {
            source: leaf.source().clone(),
            project_fanout: adapter.project_fanout,
            finish_fanout: adapter.finish_fanout,
            admitted: adapter.admitted.clone(),
            observed_before_65: adapter.observed_before_65.clone(),
        }))
    }
);

struct FramingPolicyTestAdapter {
    projected: Arc<Mutex<Vec<Vec<u8>>>>,
    record_framing: JsonlRecordFraming,
}

struct FramingPolicyTestProjector {
    projected: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl JsonlFamilyProjector for FramingPolicyTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.projected.lock().unwrap().push(record.bytes().to_vec());
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    FramingPolicyTestAdapter,
    "framing-policy-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |adapter, _leaf, _source_file, _imported_at| {
        Ok(Box::new(FramingPolicyTestProjector {
            projected: Arc::clone(&adapter.projected),
        }))
    },
    |adapter| adapter.record_framing
);

#[derive(Default)]
struct CheckpointTestAdapter {
    projection_modes: Mutex<Vec<JsonlFamilyProjectionMode>>,
}

struct OptimizedLeafTestAdapter {
    scans: AtomicUsize,
    emit_wrong_source: bool,
    emit_progress_records: bool,
}

impl JsonlFamilyAdapter for OptimizedLeafTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "optimized-leaf-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "optimized leaf test must not construct the generic projector",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &IndexBaseEventLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, u64, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        drop(leaf.open_verified()?);
        let records = if self.emit_wrong_source {
            let wrong_source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "wrong-optimized-source",
                    TypedKey::utf8("wrong").map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?,
            )
            .map_err(test_contract_error)?;
            vec![emission_test_record(&wrong_source, 0)?]
        } else if self.emit_progress_records {
            vec![
                emission_test_record(leaf.source(), 1)?,
                emission_test_record(leaf.source(), 2)?,
                emission_test_typed_record(leaf.source(), 3, "tool_call")?,
            ]
        } else {
            Vec::new()
        };
        let source_bytes = if self.emit_progress_records {
            PROGRESS_TEST_RECORDS
        } else {
            TEST_RECORD
        };
        let retained_records = u64::try_from(records.len()).unwrap_or(u64::MAX);
        let complete_records = if self.emit_progress_records { 3 } else { 1 };
        let completed_bytes = if self.emit_progress_records {
            source_bytes.len() as u64
        } else {
            0
        };
        emit_page(JsonlFamilyPublication::Replace, completed_bytes, records)?;
        let observation =
            scanner::source_observation::<CaptureError>(leaf.source(), leaf.observation())?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision(),
            Sha256::digest(source_bytes).into(),
            ScannedSourceCounts {
                complete_records,
                retained_records,
                rejected_records: 0,
                ignored_records: complete_records.saturating_sub(retained_records),
                indexed_documents: retained_records,
                certified_bytes: source_bytes.len() as u64,
            },
        )
        .map_err(test_contract_error)?;
        let terminal_proof = JsonlFamilyTerminalProof::exact_file(self, leaf, &certificate)?;
        Ok(Some(JsonlFamilyOptimizedLeafOutcome::replacement(
            certificate,
            terminal_proof,
        )))
    }
}

struct CheckpointTestProjector {
    projected_records: u64,
    resumed: bool,
}

impl JsonlFamilyProjector for CheckpointTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.resumed && self.projected_records != record.evidence().physical_ordinal() {
            return Err(CaptureError::InvalidPayload(
                "opaque checkpoint resumed from the wrong JSONL ordinal".to_owned(),
            ));
        }
        self.projected_records =
            self.projected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "checkpoint test record count overflowed",
                ))?;
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        Ok(Some(TypedKey::U64(self.projected_records)))
    }
}

impl JsonlFamilyAdapter for CheckpointTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "checkpoint-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(CheckpointTestProjector {
            projected_records: 0,
            resumed: false,
        }))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        self.projection_modes.lock().unwrap().push(mode);
        let Some(checkpoint) = checkpoint else {
            if mode == JsonlFamilyProjectionMode::Cold && base_event_lookup.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "cold checkpoint test unexpectedly received a base lookup".to_owned(),
                ));
            }
            if mode == JsonlFamilyProjectionMode::Replacement && base_event_lookup.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "replacement checkpoint test did not receive a base lookup".to_owned(),
                ));
            }
            return self.projector(leaf, source_file, imported_at);
        };
        if mode != JsonlFamilyProjectionMode::CertifiedAppend || base_event_lookup.is_none() {
            return Err(CaptureError::InvalidPayload(
                "resumed checkpoint test did not receive a base lookup".to_owned(),
            ));
        }
        let TypedKey::U64(projected_records) = checkpoint else {
            return Err(CaptureError::InvalidPayload(
                "checkpoint test state is malformed".to_owned(),
            ));
        };
        Ok(Box::new(CheckpointTestProjector {
            projected_records: *projected_records,
            resumed: true,
        }))
    }
}

fn capture_parallel_test_generation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> (IndexCaptureCommitReceipt, JsonlFamilyScannerActivity) {
    let (writer, _resident, ()) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink).unwrap()
        });
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true).unwrap());
    (commit, activity)
}

fn capture_parallel_test_generation_with_terminal_revalidation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> Result<(IndexCaptureCommitReceipt, JsonlFamilyScannerActivity)> {
    let (writer, resident, ()) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink).unwrap()
        });
    let inventory = resident
        .lock()
        .map_err(|_| CaptureError::SystemInvariant("JSONL test resident lock was poisoned"))?
        .certified_inventory
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "JSONL test capture did not certify an inventory",
        ))?;
    let valid = match revalidate_complete_inventory(adapter, root, &resident, &inventory) {
        Ok(valid) => valid,
        Err(error) if error.is_not_found() || error.is_source_changed() => false,
        Err(error) => return Err(error),
    };
    if !valid {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true)?);
    Ok((commit, activity))
}

fn capture_checkpoint_test_generation(
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> IndexCaptureCommitReceipt {
    capture_parallel_test_generation(&CheckpointTestAdapter::default(), root, index_root, workers).0
}

fn run_scheduler_test_capture(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> SourceBackedRouteResult<JsonlFamilyScannerActivity> {
    let (_writer, _resident, result) = capture_test_generation!(
        adapter,
        root,
        index_root,
        workers,
        |resident, sink| capture(adapter, root, resident, sink)
    );
    result.map(|()| jsonl_family_scanner_activity())
}

fn scheduler_test_repository(parent: &Path) -> PathBuf {
    let repository = parent.join("attributed-repository");
    fs::create_dir(&repository).unwrap();
    repository
}

fn write_scheduler_test_leaf(root: &Path, partition: u64, phase: usize, ordinal: usize) {
    fs::write(
        root.join(format!(
            "partition-{partition:02}-phase-{phase}-leaf-{ordinal}.jsonl"
        )),
        b"{\"message\":\"scheduler\"}\n",
    )
    .unwrap();
}

fn provider_checkpoints(receipt: &IndexCaptureCommitReceipt) -> Vec<Option<TypedKey>> {
    receipt
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let frontier = source.frontier().unwrap();
            FamilyCheckpoint::decode_frontier_key::<CaptureError>(frontier.checkpoint())
                .unwrap()
                .provider_checkpoint
        })
        .collect()
}

fn prepare_semantic_lifecycle_test(
    adapter: &SemanticLifecycleTestAdapter,
    root: &Path,
    index_root: &Path,
    base: Option<&CertifiedSource>,
    publications: &mut Vec<(bool, u64, usize)>,
) -> Result<leaf::PreparedLeaf<CaptureError>> {
    let inventory = adapter.discover(root)?;
    let leaf = inventory
        .leaves()
        .first()
        .ok_or(CaptureError::SystemInvariant(
            "semantic lifecycle test has no leaf",
        ))?;
    let writer = match TestLifecycle::open(index_root, ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |event| {
        if let JsonlLeafOutputEvent::Page {
            append,
            completed_bytes,
            records,
        } = event
        {
            publications.push((append, completed_bytes, records.len()));
        }
        Ok(())
    };
    prepare_leaf(
        adapter,
        leaf,
        base,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut JsonlLeafOutput::new(&mut emit),
    )
}
