use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use super::super::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_jsonl_prefix_hash_hook,
    JsonlReader,
};
use super::*;
use crate::provider::source_backed::{
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections, SourceBackedRouteResources,
};
use crate::repository_attribution::AttributionInput;
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, SessionIdentityInput, SourceAnchor,
};
use ctx_history_index::{CommitReceipt, GenerationWriter, SourceRouteIdentity, WriterOptions};

#[path = "tests/checkpoint_lifecycle.rs"]
mod checkpoint_lifecycle;
#[path = "tests/behavior.rs"]
mod behavior;

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

fn test_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

macro_rules! capture_test_generation {
    ($adapter:expr, $root:expr, $index_root:expr, $workers:expr, $capture:expr) => {{
        let resident = Mutex::new(FamilyResident::default());
        let mut writer = GenerationWriter::open($index_root, test_writer_options())
            .unwrap()
            .into_writer()
            .unwrap();
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let result = {
            let mut sink = SourceBackedGenerationSink {
                core_record_preparer: writer.core_record_preparer(),
                writer: &mut writer,
                owners: &mut owners,
                complete_inventories: &mut complete_inventories,
                route_index: 0,
                route_identity: test_route_identity(),
                resources: SourceBackedRouteResources::production($workers),
                logical_source_failures: &mut logical_source_failures,
                record_rejections: &mut record_rejections,
                applied_removals: &mut Vec::new(),
                record_progress: None,
                current_source_progress: None,
            };
            with_family_scanner_workers($workers, || $capture(&resident, &mut sink))
        };
        (writer, result)
    }};
}

struct TestAdapter;

const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";

impl JsonlFamilyAdapter for TestAdapter {
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
                    TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
                )
                .map_err(contract_error)?,
            )
            .map_err(contract_error)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                PathBuf::from(&name),
                TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
            ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "terminal leaf swap tests never project",
        ))
    }
}

fn expected_state(
    adapter: &dyn JsonlFamilyAdapter,
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
            let observation =
                leaf::source_observation(leaf.source(), checkpoint.source_observation()).unwrap();
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
                            .map_err(contract_error)?,
                    )
                    .map_err(contract_error)?,
                )
                .map_err(contract_error)?;
                leaves.push(JsonlFamilyLeaf::observe(
                    source,
                    path,
                    Arc::clone(&authority),
                    PathBuf::from(&name),
                    TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        let full_probes_before = worker
            .repository_attributor()
            .full_certification_probe_count();
        let event_time_entries_before = worker.repository_attributor().event_time_cache_len();
        if self.attribute_repository {
            let annotation = worker.repository_attributor().attribute(AttributionInput {
                activity_at_unix_ms: Some(
                    1_700_000_000_000_i64
                        .saturating_add(self.leaf.phase as i64)
                        .saturating_add(self.leaf.ordinal as i64),
                ),
                declared_tool_workdir: Some(self.repository.to_string_lossy().into_owned()),
                ..AttributionInput::default()
            });
            if annotation.repository_bindings.len() != 1 {
                return Err(CaptureError::InvalidPayload(
                    "scheduler test repository attribution did not bind".to_owned(),
                ));
            }
        }
        let full_probes_after = worker
            .repository_attributor()
            .full_certification_probe_count();
        let event_time_entries_after = worker.repository_attributor().event_time_cache_len();
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        self.0.projector(leaf, source_file, imported_at)
    }
}

struct IdentityRevisionTestAdapter {
    parser_revision: &'static str,
    revision: &'static str,
    expected_mode: JsonlFamilyProjectionMode,
}

impl JsonlFamilyAdapter for IdentityRevisionTestAdapter {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(ParallelTestProjector))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        Ok(JsonlFamilySemanticSummary::new(represented, 0, None))
    }
}

impl JsonlFamilyAdapter for SemanticLifecycleTestAdapter {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "semantic lifecycle tests require the semantic executor",
        ))
    }

    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<dyn JsonlFamilySemanticExecutor>>> {
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
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("session").map_err(contract_error)?,
    )
    .map_err(contract_error)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .map_err(contract_error)?;
    let native_item_key =
        NativeItemKey::native_id("message", TypedKey::U64(ordinal)).map_err(contract_error)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract_error)?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        ordinal,
        "message",
        "primary",
        true,
        "jsonl-emission-test-v1",
        "bounded",
    )
    .map_err(contract_error)?;
    projected.provider_session_id = Some("session".to_owned());
    projected.native_event_id = Some(TypedKey::U64(ordinal));
    projected.occurred_at_unix_ms = Some(ordinal as i64);
    projected.role = Some("user".to_owned());
    Ok(projected)
}

impl JsonlFamilyProjector for EmissionTestProjector {
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
}

impl JsonlFamilyAdapter for OptimizedLeafTestAdapter {
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "optimized leaf test must not construct the generic projector",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &BaseEventIdentityLookup,
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
                    TypedKey::utf8("wrong").map_err(contract_error)?,
                )
                .map_err(contract_error)?,
            )
            .map_err(contract_error)?;
            vec![emission_test_record(&wrong_source, 0)?]
        } else {
            Vec::new()
        };
        emit_page(JsonlFamilyPublication::Replace, 0, records)?;
        let observation = leaf::source_observation(leaf.source(), leaf.observation())?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision(),
            Sha256::digest(TEST_RECORD).into(),
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 0,
                rejected_records: 0,
                ignored_records: 1,
                indexed_documents: 0,
                certified_bytes: TEST_RECORD.len() as u64,
            },
        )
        .map_err(contract_error)?;
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> (CommitReceipt, JsonlFamilyScannerActivity) {
    let (writer, ()) = capture_test_generation!(
        adapter,
        root,
        index_root,
        workers,
        |resident, sink| capture(adapter, root, resident, sink).unwrap()
    );
    let activity = jsonl_family_scanner_activity();
    let commit = writer
        .commit_with_complete_inventory_revalidation(|_| true, |_| true)
        .unwrap();
    (commit, activity)
}

fn capture_checkpoint_test_generation(
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> CommitReceipt {
    capture_parallel_test_generation(&CheckpointTestAdapter::default(), root, index_root, workers).0
}

fn run_scheduler_test_capture(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> SourceBackedRouteResult<JsonlFamilyScannerActivity> {
    let (_writer, result) = capture_test_generation!(
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
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
    ] {
        let status = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "fixture"]] {
        let status = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    }
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

fn provider_checkpoints(receipt: &CommitReceipt) -> Vec<Option<TypedKey>> {
    receipt
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let frontier = source.frontier().unwrap();
            FamilyCheckpoint::decode_frontier_key(frontier.checkpoint())
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
) -> Result<leaf::PreparedLeaf> {
    let inventory = adapter.discover(root)?;
    let leaf = inventory
        .leaves()
        .first()
        .ok_or(CaptureError::SystemInvariant(
            "semantic lifecycle test has no leaf",
        ))?;
    let writer = GenerationWriter::open(
        index_root,
        test_writer_options(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
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
