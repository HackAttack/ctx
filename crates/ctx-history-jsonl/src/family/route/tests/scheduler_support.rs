use super::*;

pub(super) struct ParallelTestAdapter;

pub(super) struct ParallelTestProjector;

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

pub(super) struct ReplacementParallelTestAdapter;

impl_standard_jsonl_test_adapter!(
    ReplacementParallelTestAdapter,
    "replacement-parallel-test-parser-v1",
    JsonlFamilyAppendMode::Replacement,
    |_adapter, _leaf, _source_file, _imported_at| { Ok(Box::new(ParallelTestProjector)) }
);

pub(super) struct AllRejectedParallelTestAdapter {
    pub(super) reject: Arc<AtomicBool>,
}

pub(super) struct AllRejectedParallelTestProjector {
    pub(super) source: SourceKey,
    pub(super) reject: Arc<AtomicBool>,
    pub(super) rejected_records: u64,
}

impl JsonlFamilyProjector for AllRejectedParallelTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.reject.load(Ordering::SeqCst) {
            self.rejected_records =
                self.rejected_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "all-rejected test count overflowed",
                    ))?;
            return Ok(());
        }
        emit(emission_test_record(
            &self.source,
            record.evidence().physical_ordinal(),
        )?)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

impl_standard_jsonl_test_adapter!(
    AllRejectedParallelTestAdapter,
    "all-rejected-parallel-test-parser-v1",
    JsonlFamilyAppendMode::Replacement,
    |adapter, leaf, _source_file, _imported_at| {
        Ok(Box::new(AllRejectedParallelTestProjector {
            source: leaf.source().clone(),
            reject: Arc::clone(&adapter.reject),
            rejected_records: 0,
        }))
    }
);

pub(super) struct PhasedTestAdapter {
    pub(super) completed_first_phase: Arc<AtomicUsize>,
    pub(super) second_phase_started_early: Arc<AtomicBool>,
}

pub(super) struct PhasedTestProjector {
    pub(super) phase: usize,
    pub(super) completed_first_phase: Arc<AtomicUsize>,
    pub(super) second_phase_started_early: Arc<AtomicBool>,
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
pub(super) struct SchedulerLeafState {
    pub(super) partition: u64,
    pub(super) phase: usize,
    pub(super) ordinal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SchedulerStateEvent {
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

pub(super) struct SchedulerStateTestAdapter {
    pub(super) repository: PathBuf,
    pub(super) attributed_partitions: Vec<u64>,
    pub(super) failing_leaf: Option<SchedulerLeafState>,
    pub(super) parallel_frontier: Option<(u64, usize, Arc<std::sync::Barrier>)>,
    pub(super) events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

pub(super) struct UnpartitionedSchedulerStateTestAdapter(pub(super) SchedulerStateTestAdapter);

pub(super) struct SchedulerStateTestProjector {
    pub(super) leaf: SchedulerLeafState,
    pub(super) repository: PathBuf,
    pub(super) attribute_repository: bool,
    pub(super) fail: bool,
    pub(super) parallel_frontier: Option<Arc<std::sync::Barrier>>,
    pub(super) events: Arc<Mutex<Vec<SchedulerStateEvent>>>,
}

pub(super) fn scheduler_leaf_state(leaf: &JsonlFamilyLeaf) -> Result<SchedulerLeafState> {
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

pub(super) struct IdentityRevisionTestAdapter {
    pub(super) parser_revision: &'static str,
    pub(super) revision: &'static str,
    pub(super) expected_mode: JsonlFamilyProjectionMode,
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
