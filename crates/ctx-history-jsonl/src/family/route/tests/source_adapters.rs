use super::*;

pub(super) struct TestAdapter;

pub(super) const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";
pub(super) const PROGRESS_TEST_RECORDS: &[u8] =
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
pub(super) struct TerminalLeafSwapTestAdapter {
    pub(super) selected: PathBuf,
    pub(super) outside: PathBuf,
    pub(super) enabled: AtomicBool,
    pub(super) swapped: AtomicBool,
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

pub(super) fn expected_state(
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

pub(super) struct FrozenMultiRootTestAdapter {
    pub(super) roots: Vec<PathBuf>,
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

pub(super) struct TerminalRootSwapTestAdapter {
    pub(super) root: PathBuf,
    pub(super) discoveries: AtomicUsize,
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

pub(super) fn expected_source(resident: &FamilyResident) -> CertifiedSource {
    resident
        .terminal_sources
        .values()
        .next()
        .unwrap()
        .certificate
        .clone()
}

pub(super) use impl_standard_jsonl_test_adapter;
