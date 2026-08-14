use super::*;

const PARSER_V1: &str = "no-io-document-v1";
const PARSER_V2: &str = "no-io-document-v2";
const SCHEMA_V1: &str = "no-io-document-schema-v1";
const SCHEMA_V2: &str = "no-io-document-schema-v2";

#[derive(Debug, Clone, PartialEq, Eq)]
struct LifecycleLeaf {
    physical_id: u8,
    logical_id: u8,
    revision: u8,
    body: String,
}

impl LifecycleLeaf {
    fn source(&self, schema_v2: bool) -> SourceKey {
        document_source(
            self.logical_id,
            if schema_v2 { SCHEMA_V2 } else { SCHEMA_V1 },
        )
    }

    fn physical_fingerprint(&self) -> DocumentLeafFingerprint {
        let mut fingerprint = [self.physical_id; 32];
        fingerprint[1] = self.revision;
        DocumentLeafFingerprint::new(fingerprint)
    }

    fn logical_fingerprint(&self) -> [u8; 32] {
        let mut fingerprint = [0; 32];
        for (index, byte) in self.body.bytes().enumerate() {
            fingerprint[index % 32] ^= byte;
        }
        fingerprint
    }
}

#[derive(Default)]
struct LifecycleState {
    leaves: Vec<LifecycleLeaf>,
    durable_replay: bool,
    bind_exact_descriptor: bool,
    parser_v2: bool,
    schema_v2: bool,
    independent_workers: Option<usize>,
    scan_counts: HashMap<u8, usize>,
    scan_barrier: Option<Arc<Barrier>>,
    active_scans: usize,
    peak_scans: usize,
    mutate_before_scan: Option<u8>,
    mutate_on_revalidate: bool,
    unavailable_leaf: Option<u8>,
}

#[derive(Clone)]
struct LifecycleAdapter {
    state: Arc<Mutex<LifecycleState>>,
}

impl LifecycleAdapter {
    fn new(leaves: Vec<LifecycleLeaf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(LifecycleState {
                leaves,
                durable_replay: true,
                ..LifecycleState::default()
            })),
        }
    }

    fn tree(state: &LifecycleState) -> CompleteDocumentTree<LifecycleLeaf, [u8; 32]> {
        let leaves = state
            .leaves
            .iter()
            .cloned()
            .map(|leaf| {
                ObservedDocumentLeaf::with_durable_replay(
                    leaf.physical_fingerprint(),
                    leaf,
                    state.durable_replay,
                )
            })
            .collect::<Vec<_>>();
        let mut tree_fingerprint = [0; 32];
        for observed in &leaves {
            let fingerprint = observed.fingerprint.as_bytes();
            for (target, byte) in tree_fingerprint.iter_mut().zip(fingerprint) {
                *target ^= byte;
            }
        }
        CompleteDocumentTree::new(tree_fingerprint, leaves, tree_fingerprint)
    }

    fn driver(&self) -> crate::SourceBackedRouteDriver<FakeLifecycle, ()> {
        crate::replacement_document_tree_driver(
            DocumentInventoryAuthority::new("parallel-leaf-test".to_owned(), [0x31; 32]),
            self.clone(),
        )
    }

    fn reset_work(&self) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.active_scans, 0);
        state.scan_counts.clear();
        state.peak_scans = 0;
    }

    fn scan_count(&self, physical_id: u8) -> usize {
        self.state
            .lock()
            .unwrap()
            .scan_counts
            .get(&physical_id)
            .copied()
            .unwrap_or_default()
    }

    fn total_scans(&self) -> usize {
        self.state.lock().unwrap().scan_counts.values().sum()
    }

    fn peak_scans(&self) -> usize {
        self.state.lock().unwrap().peak_scans
    }

    fn replace(&self, physical_id: u8, revision: u8, body: &str) {
        let mut state = self.state.lock().unwrap();
        let leaf = state
            .leaves
            .iter_mut()
            .find(|leaf| leaf.physical_id == physical_id)
            .unwrap();
        leaf.revision = revision;
        leaf.body = body.to_owned();
    }

    fn touch(&self, physical_id: u8, revision: u8) {
        let body = self
            .state
            .lock()
            .unwrap()
            .leaves
            .iter()
            .find(|leaf| leaf.physical_id == physical_id)
            .unwrap()
            .body
            .clone();
        self.replace(physical_id, revision, &body);
    }

    fn use_parallel(&self, workers: usize) {
        self.state.lock().unwrap().independent_workers = Some(workers);
    }

    fn install_barrier(&self, workers: usize) {
        self.state.lock().unwrap().scan_barrier = Some(Arc::new(Barrier::new(workers)));
    }

    fn clear_barrier(&self) {
        self.state.lock().unwrap().scan_barrier = None;
    }
}

struct ActiveScan(Arc<Mutex<LifecycleState>>);

impl Drop for ActiveScan {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.active_scans = state.active_scans.saturating_sub(1);
    }
}

impl ReplacementDocumentTree for LifecycleAdapter {
    type Lifecycle = FakeLifecycle;
    type Spool = NoIoDocumentSpool;
    type RouteControl = ();
    type Leaf = LifecycleLeaf;
    type TreeAuthority = [u8; 32];

    fn parser_revision(&self) -> &'static str {
        if self.state.lock().unwrap().parser_v2 {
            PARSER_V2
        } else {
            PARSER_V1
        }
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == "parallel-leaf-test"
            && matches!(source.schema_variant(), SCHEMA_V1 | SCHEMA_V2)
    }

    fn leaf_execution_policy(&self) -> crate::DocumentLeafExecutionPolicy {
        match self.state.lock().unwrap().independent_workers {
            Some(workers) => crate::DocumentLeafExecutionPolicy::IndependentCapped(workers),
            None => crate::DocumentLeafExecutionPolicy::Serial,
        }
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Ok(leaf.source(self.state.lock().unwrap().schema_v2))
    }

    fn durable_replay_source(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        if self.state.lock().unwrap().bind_exact_descriptor {
            self.independent_leaf_source(authority, leaf).map(Some)
        } else {
            Ok(None)
        }
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        Ok(Self::tree(&self.state.lock().unwrap()))
    }

    fn scan_changed(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut crate::ChangedDocumentSink<'_, '_, Self::Lifecycle, Self::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let barrier = {
            let mut state = self.state.lock().unwrap();
            state.active_scans += 1;
            state.peak_scans = state.peak_scans.max(state.active_scans);
            state.scan_barrier.clone()
        };
        let _active = ActiveScan(Arc::clone(&self.state));
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let (live, schema_v2, parser_revision) = {
            let mut state = self.state.lock().unwrap();
            if state.mutate_before_scan == Some(leaf.physical_id) {
                let live = state
                    .leaves
                    .iter_mut()
                    .find(|candidate| candidate.physical_id == leaf.physical_id)
                    .unwrap();
                live.revision = live.revision.saturating_add(1);
                state.mutate_before_scan = None;
            }
            if state.unavailable_leaf == Some(leaf.physical_id) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "injected logical source failure",
                ));
            }
            let live = state
                .leaves
                .iter()
                .find(|candidate| candidate.physical_id == leaf.physical_id)
                .cloned()
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "document leaf disappeared before scan",
                    )
                })?;
            if live != *leaf {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "document leaf changed between observation and scan",
                ));
            }
            *state.scan_counts.entry(leaf.physical_id).or_default() += 1;
            (
                live,
                state.schema_v2,
                if state.parser_v2 {
                    PARSER_V2
                } else {
                    PARSER_V1
                },
            )
        };
        let source = live.source(schema_v2);
        sink.begin_source(source.clone())?;
        sink.emit_core_record(test_core_record(&source, 0, live.revision))?;
        let observation_revision = if self.state.lock().unwrap().durable_replay {
            vec![live.revision]
        } else {
            live.logical_fingerprint().to_vec()
        };
        let observation = SourceObservation::new(
            source.clone(),
            "no-io-document-revision-v1",
            observation_revision,
        )
        .unwrap();
        Ok(DocumentSourceTerminal {
            source,
            opening: observation.clone(),
            closing: observation,
            parser_revision,
            content_digest: live.logical_fingerprint(),
            counts: ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 1,
                ..ScannedSourceCounts::default()
            },
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let mut state = self.state.lock().unwrap();
        if state.mutate_on_revalidate {
            state.leaves[0].revision = state.leaves[0].revision.saturating_add(1);
            state.mutate_on_revalidate = false;
        }
        let current = Self::tree(&state).tree_fingerprint;
        if current == tree.tree_fingerprint {
            Ok(current)
        } else {
            Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "document tree changed before terminal revalidation",
            ))
        }
    }
}

fn document_source(logical_id: u8, schema: &str) -> SourceKey {
    SourceKey::derive(
        "parallel-leaf-test",
        "parallel_leaf_fixture",
        schema,
        1,
        SourceAnchor::CatalogLineage([logical_id; 32]),
    )
    .unwrap()
}

fn leaf(id: u8) -> LifecycleLeaf {
    LifecycleLeaf {
        physical_id: id,
        logical_id: id,
        revision: 1,
        body: format!("document {id}"),
    }
}

fn run(adapter: &LifecycleAdapter, base: &[CertifiedSource], workers: usize) -> SinkHarness {
    let lifecycle = FakeLifecycle {
        base_sources: base.to_vec(),
        ..FakeLifecycle::default()
    };
    let mut harness = SinkHarness::with_lifecycle(lifecycle);
    harness.leaf_worker_budget = workers;
    adapter.driver().scan(&mut harness.sink()).unwrap();
    harness
}

fn run_error(
    adapter: &LifecycleAdapter,
    base: &[CertifiedSource],
    workers: usize,
) -> (SourceBackedRouteError, SinkHarness) {
    let lifecycle = FakeLifecycle {
        base_sources: base.to_vec(),
        ..FakeLifecycle::default()
    };
    let mut harness = SinkHarness::with_lifecycle(lifecycle);
    harness.leaf_worker_budget = workers;
    let error = adapter.driver().scan(&mut harness.sink()).unwrap_err();
    (error, harness)
}

fn sources(harness: &SinkHarness) -> Vec<CertifiedSource> {
    let mut sources = harness.writer.certified_sources.clone();
    sources.sort_by_key(|source| source.observation().source().identity().digest());
    sources
}

#[test]
fn active_source_family_contract_document_full_snapshot_decoder_is_the_strict_persisted_frontier_authority(
) {
    let source = document_source(1, SCHEMA_V1);
    let observation = SourceObservation::new(source, "document-revision", vec![1]).unwrap();
    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 7,
        ..ScannedSourceCounts::default()
    };
    let certify = |frontier| {
        CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation.clone(),
            PARSER_V1,
            [0x22; 32],
            counts,
            frontier,
        )
        .unwrap()
    };
    let valid = certify(Some(
        crate::document_full_snapshot_frontier(
            DocumentLeafFingerprint::new([0x11; 32]),
            counts.certified_bytes,
            [0x22; 32],
        )
        .unwrap(),
    ));
    let decoded = crate::decode_document_full_snapshot_checkpoint(&valid).unwrap();
    assert_eq!(decoded.physical_fingerprint(), [0x11; 32]);
    assert_eq!(decoded.logical_fingerprint(), [0x22; 32]);

    assert_eq!(
        crate::decode_document_full_snapshot_checkpoint(&certify(None)),
        Err(crate::DocumentFullSnapshotCheckpointError::MissingFrontier)
    );
    for (frontier, expected) in [
        (
            SourceFrontier::new(
                "provider-owned",
                TypedKey::Bytes(vec![0; 32]),
                7,
                [0x22; 32],
            )
            .unwrap(),
            crate::DocumentFullSnapshotCheckpointError::UnexpectedFrontierKind,
        ),
        (
            SourceFrontier::new(
                "ctx-document-full-snapshot-v1",
                TypedKey::U64(1),
                7,
                [0x22; 32],
            )
            .unwrap(),
            crate::DocumentFullSnapshotCheckpointError::NonByteCheckpoint,
        ),
        (
            SourceFrontier::new(
                "ctx-document-full-snapshot-v1",
                TypedKey::Bytes(vec![0; 31]),
                7,
                [0x22; 32],
            )
            .unwrap(),
            crate::DocumentFullSnapshotCheckpointError::InvalidFingerprint,
        ),
    ] {
        assert_eq!(
            crate::decode_document_full_snapshot_checkpoint(&certify(Some(frontier))),
            Err(expected)
        );
    }
}

#[test]
fn active_source_family_contract_document_serial_parallel_have_parity_and_bounded_peak_work() {
    let leaves = (0..8).map(leaf).collect::<Vec<_>>();
    let serial = LifecycleAdapter::new(leaves.clone());
    serial.use_parallel(1);
    serial.install_barrier(1);
    let parallel = LifecycleAdapter::new(leaves);
    parallel.use_parallel(4);
    parallel.install_barrier(4);

    let serial_cold = run(&serial, &[], 4);
    let parallel_cold = run(&parallel, &[], 4);
    assert_eq!(sources(&parallel_cold), sources(&serial_cold));
    assert_eq!(
        parallel_cold.writer.records.len(),
        serial_cold.writer.records.len()
    );
    assert!(sources(&serial_cold).iter().all(|certificate| parallel_cold
        .writer
        .records
        .iter()
        .any(|record| record
            .source
            .exact_descriptor_eq(certificate.observation().source()))));
    assert_eq!(serial.peak_scans(), 1);
    assert_eq!(parallel.peak_scans(), 4);
    assert_eq!(serial.total_scans(), 8);
    assert_eq!(parallel.total_scans(), 8);

    serial.clear_barrier();
    parallel.clear_barrier();
    serial.reset_work();
    parallel.reset_work();
    let serial_replay = run(&serial, &sources(&serial_cold), 4);
    let parallel_replay = run(&parallel, &sources(&parallel_cold), 4);
    assert_eq!(sources(&parallel_replay), sources(&serial_replay));
    assert_eq!(serial.total_scans(), 0);
    assert_eq!(parallel.total_scans(), 0);
}

#[test]
fn active_source_family_contract_document_durable_replay_is_bound_to_the_exact_descriptor_and_parser(
) {
    let adapter = LifecycleAdapter::new(vec![leaf(1)]);
    {
        let mut state = adapter.state.lock().unwrap();
        state.bind_exact_descriptor = true;
    }
    let cold = run(&adapter, &[], 1);
    let base = sources(&cold);
    adapter.reset_work();
    adapter.state.lock().unwrap().schema_v2 = true;
    let replaced = run(&adapter, &base, 1);
    assert_eq!(adapter.scan_count(1), 1);
    assert_eq!(sources(&replaced).len(), 1);
    assert_eq!(
        sources(&replaced)[0]
            .observation()
            .source()
            .schema_variant(),
        SCHEMA_V2
    );

    adapter.reset_work();
    adapter.state.lock().unwrap().parser_v2 = true;
    let parser_replaced = run(&adapter, &sources(&replaced), 1);
    assert_eq!(adapter.scan_count(1), 1);
    assert_eq!(sources(&parser_replaced)[0].parser_revision(), PARSER_V2);
}

#[test]
fn active_source_family_contract_document_add_change_delete_failure_carry_forward_and_reappearance_are_exact(
) {
    let adapter = LifecycleAdapter::new(vec![leaf(1), leaf(2)]);
    adapter.use_parallel(4);
    let cold = run(&adapter, &[], 4);
    assert_eq!(sources(&cold).len(), 2);

    adapter.reset_work();
    let replay = run(&adapter, &sources(&cold), 4);
    assert_eq!(adapter.total_scans(), 0);

    adapter.replace(2, 2, "changed document 2");
    adapter.reset_work();
    let changed = run(&adapter, &sources(&replay), 4);
    assert_eq!(adapter.scan_count(1), 0);
    assert_eq!(adapter.scan_count(2), 1);

    adapter.state.lock().unwrap().leaves.push(leaf(3));
    adapter.reset_work();
    let added = run(&adapter, &sources(&changed), 4);
    assert_eq!(sources(&added).len(), 3);
    assert_eq!(adapter.scan_count(3), 1);

    let deleted_source = document_source(1, SCHEMA_V1);
    adapter
        .state
        .lock()
        .unwrap()
        .leaves
        .retain(|leaf| leaf.logical_id != 1);
    adapter.reset_work();
    let deleted = run(&adapter, &sources(&added), 4);
    assert!(deleted.applied_removals.iter().any(|removal| removal
        .deletion
        .source()
        .exact_descriptor_eq(&deleted_source)));

    adapter.state.lock().unwrap().leaves.push(leaf(1));
    let reappeared = run(&adapter, &sources(&deleted), 4);
    assert_eq!(sources(&reappeared).len(), 3);

    adapter.replace(2, 3, "unavailable replacement");
    adapter.state.lock().unwrap().unavailable_leaf = Some(2);
    let failed = run(&adapter, &sources(&reappeared), 4);
    assert_eq!(failed.logical_source_failures.total(), 1);
    assert!(failed.logical_source_failures.failures()[0].carried_forward);
    assert!(sources(&failed).iter().any(|certificate| certificate
        .observation()
        .source()
        .exact_descriptor_eq(&document_source(2, SCHEMA_V1))));
}

#[test]
fn active_source_family_contract_document_logical_snapshot_noop_and_parallel_replay_scan_once() {
    let adapter = LifecycleAdapter::new((0..8).map(leaf).collect());
    {
        let mut state = adapter.state.lock().unwrap();
        state.durable_replay = false;
    }
    adapter.use_parallel(4);
    adapter.install_barrier(4);
    let cold = run(&adapter, &[], 4);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(adapter.peak_scans(), 4);

    adapter.clear_barrier();
    for id in 0..8 {
        adapter.touch(id, 2);
    }
    adapter.reset_work();
    let logical_noop = run(&adapter, &sources(&cold), 4);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(logical_noop.writer.records.len(), 0);
    assert_eq!(sources(&logical_noop), sources(&cold));

    adapter.replace(3, 3, "logical replacement");
    adapter.reset_work();
    let changed = run(&adapter, &sources(&logical_noop), 4);
    assert_eq!(adapter.total_scans(), 8);
    assert_eq!(changed.writer.records.len(), 1);
}

#[test]
fn active_source_family_contract_document_races_duplicates_and_terminal_inventory_mutation_fail_closed(
) {
    let adapter = LifecycleAdapter::new(vec![leaf(1)]);
    let cold = run(&adapter, &[], 1);

    adapter.replace(1, 2, "source race");
    adapter.state.lock().unwrap().mutate_before_scan = Some(1);
    let (race, race_harness) = run_error(&adapter, &sources(&cold), 1);
    assert_eq!(race.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert!(race_harness.writer.certified_sources.is_empty());

    let duplicate_physical = LifecycleAdapter::new(vec![leaf(8), leaf(8)]);
    let (duplicate, duplicate_harness) = run_error(&duplicate_physical, &[], 1);
    assert_eq!(duplicate.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert!(duplicate_harness.writer.records.is_empty());

    let mut first = leaf(10);
    first.logical_id = 42;
    let mut second = leaf(11);
    second.logical_id = 42;
    let duplicate_source = LifecycleAdapter::new(vec![first, second]);
    let (duplicate, _) = run_error(&duplicate_source, &[], 1);
    assert_eq!(duplicate.kind, SourceBackedRouteErrorKind::SourceChanged);

    let terminal = LifecycleAdapter::new(vec![leaf(20)]);
    let driver = terminal.driver();
    let mut scanned = SinkHarness::open(Path::new("/unused"));
    driver.scan(&mut scanned.sink()).unwrap();
    terminal.state.lock().unwrap().mutate_on_revalidate = true;
    let inventory = scanned.complete_inventories[0].inventory.clone();
    let certificate = scanned.writer.certified_sources[0].clone();
    assert!(driver
        .revalidate(crate::SourceBackedRevalidationTarget::Source(&certificate))
        .unwrap());
    assert!(!driver
        .revalidate_complete_inventory(&inventory)
        .unwrap()
        .unwrap());
}
