use super::*;
use std::fs;

use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs,
    SourceBackedSelectorAuthority,
};
use ctx_history_capture_model::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, TypedKey,
};

struct TestPublishedState;

impl PublishedSourceBackedStatePort for TestPublishedState {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState> {
        let index_root = source_backed_index_root(data_root);
        if !index_root.is_dir() {
            return Ok(PublishedSourceBackedState {
                verified_index: None,
                explicit_source_catalog: None,
                catalog_route_bindings: Vec::new(),
                route_controls: BTreeMap::new(),
            });
        }
        let verified_index = match VerifiedIndex::open(&index_root) {
            Ok(index) => Some(index),
            Err(IndexError::MissingActiveGenerationPointer) => None,
            Err(error)
                if ctx_history_index::generation_incompatibility_requires_rebuild(&error) =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        let (explicit_source_catalog, catalog_route_bindings, route_controls) = if let Some(index) =
            verified_index
                .as_ref()
                .filter(|index| index.publication_metadata().is_some())
        {
            let metadata = SourceBackedPublicationMetadata::decode(index)?;
            let receipt = published_refresh_receipt_for_index(&metadata.response_value(), index)?;
            (
                receipt.published_explicit_source_catalog,
                receipt.catalog_route_bindings,
                metadata.route_controls,
            )
        } else {
            (None, Vec::new(), BTreeMap::new())
        };
        Ok(PublishedSourceBackedState {
            verified_index,
            explicit_source_catalog,
            catalog_route_bindings,
            route_controls,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        "test-refresh",
        RefreshOperation::Refresh,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        &SourceBackedRefreshCoveredPublication::default(),
        &TestPublishedState,
        report_progress,
    )
}

fn discovery_fixture(root: &Path) -> (PathBuf, PathBuf, DiscoveryContext) {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    (home, cwd, discovery)
}

fn run_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    data_root: &Path,
    index_root: &Path,
) -> Result<SourceBackedRefreshPublication> {
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    refresh_all_provider_sources(
        discovery,
        report,
        StdDuration::ZERO,
        data_root,
        index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
}

fn publication_pin_source_with_anchor(anchor: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([anchor; 32]),
    )
    .unwrap()
}

fn publication_pin_record(source: &SourceKey) -> CoreRecord {
    let native_session = TypedKey::utf8("publication-pin-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item =
        NativeItemKey::native_id("message", TypedKey::utf8("publication-pin-event").unwrap())
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        0,
        "message",
        "primary",
        true,
        "publication-pin-test-v1",
        "exact publication pin fixture",
    )
    .unwrap();
    record.provider_session_id = Some("publication-pin-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.role = Some("user".to_owned());
    record.validate_contract().unwrap();
    record
}

fn publication_pin_certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "publication-pin-test-v1",
        [0x92; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 128,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish_pin_source(index_root: &Path, source: SourceKey) -> String {
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(publication_pin_record(&source))
        .unwrap();
    writer
        .certify_source(publication_pin_certificate(&source))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        route_results: Vec::new(),
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
        generation_id: generation_id.into(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 128,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            indexed_documents: 1,
            complete_records: 1,
            retained_records: 1,
            certified_source_bytes: 128,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings::default(),
    }
}

#[test]
fn query_readiness_accepts_nonempty_generation_without_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x95));
    let verified = VerifiedIndex::open(temp.path()).unwrap();

    assert_eq!(verified.generation_id(), generation_id);
    assert!(verified.publication_metadata().is_none());
    assert_eq!(
        verify_generation_query_readiness(&verified).unwrap(),
        GenerationQueryReadiness::Ready
    );
}

#[test]
fn query_readiness_decodes_metadata_before_certifying_generation() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x96));
    let publication = test_publication(generation_id.clone());
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        generation_id.clone(),
        &publication,
    )
    .unwrap();
    let encoded = SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: "query-readiness-metadata".to_owned(),
        operation: RefreshOperation::Refresh,
        refresh_scope: SourceBackedRefreshScope::All,
        receipt: receipt.to_json(),
        route_observations: BTreeMap::new(),
        route_controls: BTreeMap::new(),
    }
    .encode()
    .unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let verified = writer
        .republish_current_publication_metadata(&generation_id, encoded)
        .unwrap();

    assert!(verified.publication_metadata().is_some());
    assert_eq!(
        verify_generation_query_readiness(&verified).unwrap(),
        GenerationQueryReadiness::Ready
    );

    let mut invalid_metadata: Value =
        serde_json::from_slice(verified.publication_metadata().unwrap()).unwrap();
    invalid_metadata["version"] = json!(99);
    drop(verified);
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let invalid = writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&invalid_metadata).unwrap(),
        )
        .unwrap();
    assert!(format!(
        "{:#}",
        verify_generation_query_readiness(&invalid).unwrap_err()
    )
    .contains("unsupported Core source-refresh publication metadata version"));
}

#[path = "tests/execution_path.rs"]
mod execution_path;
#[path = "tests/receipt.rs"]
mod receipt_tests;
#[path = "tests/registry_policy.rs"]
mod registry_policy;
#[path = "tests/settings_upgrade.rs"]
mod settings_upgrade;
