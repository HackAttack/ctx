use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_capture_runtime::{
    CaptureLifecycleSink, DocumentRecordSpool, ReplacementDocumentTree, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedRouteWatchTargets, SourceBackedSelectorAuthority,
};
use ctx_history_core::{CaptureProvider, CertifiedSource, TypedKey};
use ctx_history_source_discovery::{LingmaDiscoveredInventory, LingmaDiscoveryUnavailable};

use sha2::Digest;

use crate::provider::providers::lingma::native_path::{
    reject_duplicate_paths as reject_duplicate_lingma_paths, scan_lingma_snapshot_v0,
    LingmaDatabaseSourceV0, LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0,
    LingmaSourceInventoryV0, LINGMA_SOURCE_BACKED_PARSER_REVISION,
};
use crate::provider::providers::shelley::native_path::source_backed::{
    discover_shelley_source_backed_exact_cwd, ShelleySourceBackedAdapter,
    SHELLEY_SOURCE_PARSER_REVISION,
};
use crate::{
    provider::providers::astrbot::native_path::source_backed::{
        scan_astrbot_snapshot_v0, AstrBotSourceBackedErrorV0, AstrBotSourceBackedInventoryV0,
        AstrBotSourceBackedResultV0, AstrBotSourceBackedSourceV0,
        PARSER_REVISION as ASTRBOT_SOURCE_BACKED_PARSER_REVISION,
    },
    provider::source_backed::family::document::ChangedDocumentSink,
    provider::source_backed::{combine_primary_and_cleanup_route_errors, route_error},
    provider_sources::SqliteSourceReadSnapshot,
};

use super::*;

mod crush;
mod shared;

pub use crush::crush_registration;
pub use ctx_history_core::SourceAnchor;
use shared::{
    sqlite_inventory_authority_fingerprint, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
    SqliteInventoryDocumentAdapter, SqliteInventoryProvider,
};

pub type WatchTargets =
    Box<dyn Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync + 'static>;

/// Complete provider-owned registration contract. Capture consumes this
/// fragment only to bind its concrete lifecycle and install one executable
/// route; all provider selection and watch authority is fixed here.
pub struct SqliteInventoryRegistration<A> {
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
    watch_targets: Option<WatchTargets>,
}

impl<A> SqliteInventoryRegistration<A> {
    fn new(
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        selector_authority: SourceBackedSelectorAuthority,
        adapter: A,
        watch_targets: Option<WatchTargets>,
    ) -> Self {
        Self {
            source,
            selection,
            selector_authority,
            adapter,
            watch_targets,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        ProviderSource,
        SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
        A,
        Option<WatchTargets>,
    ) {
        (
            self.source,
            self.selection,
            self.selector_authority,
            self.adapter,
            self.watch_targets,
        )
    }
}

pub fn hermes_automatic_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    L::PinnedAppendBase: Clone + Send + Sync + 'static,
    S: DocumentRecordSpool,
{
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(CaptureError::InvalidPayload(
            "manual Hermes registration requires persistent explicit catalog lineage".to_owned(),
        ));
    }
    let candidate =
        crate::provider::providers::hermes::source_backed::HermesSourceCandidate::automatic(
            data_root,
            source.clone(),
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        crate::provider::providers::hermes::source_backed::replacement::HermesDocumentAdapter::new(
            candidate,
        ),
        None,
    ))
}

pub fn hermes_explicit_registration<L, S>(
    source: ProviderSource,
    data_root: &Path,
    anchor: SourceAnchor,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    L::PinnedAppendBase: Clone + Send + Sync + 'static,
    S: DocumentRecordSpool,
{
    let candidate =
        crate::provider::providers::hermes::source_backed::hermes_source_backed_explicit(
            data_root,
            source.path.clone(),
            anchor,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SqliteInventoryRegistration::new(
        source,
        SourceBackedRouteSelection::ExplicitManual,
        SourceBackedSelectorAuthority::ExplicitPath,
        crate::provider::providers::hermes::source_backed::replacement::HermesDocumentAdapter::new(
            candidate,
        ),
        None,
    ))
}

fn sqlite_inventory_watch_targets<'a>(
    databases: impl IntoIterator<Item = &'a Path>,
) -> SourceBackedRouteWatchTargets {
    let mut targets = SourceBackedRouteWatchTargets::default();
    for database in databases {
        targets.sqlite_databases.insert(database.to_path_buf());
        if let Some(parent) = database.parent() {
            targets.authority_paths.insert(parent.to_path_buf());
        }
    }
    targets
}

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn astrbot_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_primary = source.path.clone();
    let watch_discovery = discovery.clone();
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        AstrBotInventoryProvider { discovery },
    );
    SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
        Some(Box::new(move || {
            let mut targets = AstrBotSourceBackedInventoryV0::discover(&watch_discovery)
                .ok()
                .map(|inventory| {
                    sqlite_inventory_watch_targets(
                        inventory
                            .sources()
                            .iter()
                            .map(AstrBotSourceBackedSourceV0::path),
                    )
                })
                .unwrap_or_default();
            // Retain exact provider authority roots even when an inventory probe
            // fails. That keeps warm observation indeterminate while ensuring a
            // healthy watcher still dirties the route for selected-root changes,
            // launcher-instance changes, and newly created finite leaves.
            if let Some(parent) = watch_primary.parent() {
                targets.authority_paths.insert(parent.to_path_buf());
            }
            targets.authority_paths.insert(
                watch_discovery
                    .home()
                    .join(".astrbot_launcher")
                    .join("instances"),
            );
            Some(targets)
        })),
    )
}

pub struct AstrBotInventoryProvider {
    discovery: DiscoveryContext,
}

impl<L, S> SqliteInventoryProvider<L, S> for AstrBotInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = AstrBotSourceBackedSourceV0;

    fn parser_revision(&self) -> &'static str {
        ASTRBOT_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = AstrBotSourceBackedInventoryV0::discover(&self.discovery)
            .map_err(astrbot_inventory_route_error)?;
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(inventory.observation())?;
        let leaves = inventory
            .sources()
            .iter()
            .cloned()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source_key().clone(),
                physical_locator: leaf.path().to_path_buf(),
                provider_leaf: leaf,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint,
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut sink_failure = None;
        let certificate = scan_astrbot_snapshot_v0(leaf, snapshot, &mut |record| {
            if let Err(error) = sink.emit_core_record(record) {
                let detail = error.to_string();
                sink_failure = Some(error);
                return Err(
                    crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0::Capture(
                        CaptureError::InvalidPayload(detail),
                    ),
                );
            }
            Ok(())
        });
        astrbot_scan_route_result(sink_failure, certificate)
    }
}

fn astrbot_scan_route_result(
    sink_failure: Option<SourceBackedRouteError>,
    certificate: AstrBotSourceBackedResultV0<CertifiedSource>,
) -> SourceBackedRouteResult<CertifiedSource> {
    if let Some(primary) = sink_failure {
        if let Err(AstrBotSourceBackedErrorV0::SnapshotCleanup { cleanup, .. }) = certificate {
            return Err(combine_primary_and_cleanup_route_errors(
                primary,
                sqlite_source_route_error(cleanup),
            ));
        }
        return Err(primary);
    }
    certificate.map_err(astrbot_inventory_route_error)
}

fn astrbot_inventory_route_error(
    error: crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0,
) -> SourceBackedRouteError {
    use crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0;
    if let AstrBotSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            astrbot_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
    let kind = match &error {
        AstrBotSourceBackedErrorV0::IncompleteInventory { .. } => {
            SourceBackedRouteErrorKind::Unavailable
        }
        AstrBotSourceBackedErrorV0::SqliteSource(error) => sqlite_source_route_error_kind(error),
        AstrBotSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

/// Registers Shelley only when the caller retains the exact CWD that selected
/// `shelley.db`. No branch or fallback CWD is inferred.
pub fn shelley_registration<L, S>(
    source: ProviderSource,
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let exact_cwd = exact_cwd.into();
    let adapter = discover_shelley_source_backed_exact_cwd(data_root, &exact_cwd)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "the exact Shelley CWD no longer contains an admitted database".to_owned(),
            )
        })?;
    if adapter.database_path() != source.path {
        return Err(CaptureError::InvalidPayload(
            "the Shelley source path does not belong to the supplied exact CWD".to_owned(),
        ));
    }
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        ShelleyInventoryProvider { exact_cwd, adapter },
    );
    Ok(SqliteInventoryRegistration::new(
        source,
        SourceBackedRouteSelection::Automatic,
        SourceBackedSelectorAuthority::ExactCwd,
        adapter,
        None,
    ))
}

pub struct ShelleyInventoryProvider {
    exact_cwd: PathBuf,
    adapter: ShelleySourceBackedAdapter,
}

impl<L, S> SqliteInventoryProvider<L, S> for ShelleyInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = ShelleySourceBackedAdapter;

    fn parser_revision(&self) -> &'static str {
        SHELLEY_SOURCE_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let mut authority = sha2::Sha256::new();
        authority.update(b"ctx.shelley-exact-cwd-inventory-v1\0");
        authority.update(self.exact_cwd.as_os_str().as_encoded_bytes());
        let leaf = self.adapter.clone();
        let leaves = match std::fs::symlink_metadata(leaf.database_path()) {
            Ok(_) => vec![SqliteInventoryCatalogLeaf {
                source: leaf.source().clone(),
                physical_locator: leaf.database_path().to_path_buf(),
                provider_leaf: leaf,
            }],
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    format!("Shelley exact-CWD inventory is unavailable: {error}"),
                ));
            }
        };
        Ok(SqliteInventoryCatalog {
            authority_fingerprint: authority.finalize().into(),
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut scan = leaf
            .start_snapshot_scan(snapshot)
            .map_err(shelley_inventory_route_error)?;
        loop {
            let page = match scan.next_page() {
                Ok(Some(page)) => page,
                Ok(None) => break,
                Err(primary) => {
                    let primary = shelley_inventory_route_error(primary);
                    return Err(abort_shelley_inventory_scan(scan, primary));
                }
            };
            for document in page.documents {
                if let Err(primary) = sink.emit_core_record(document) {
                    return Err(abort_shelley_inventory_scan(scan, primary));
                }
            }
        }
        Ok(scan
            .finish()
            .map_err(shelley_inventory_route_error)?
            .certificate)
    }
}

pub fn lingma_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let databases = databases
        .into_iter()
        .map(|(path, lineage)| LingmaDatabaseSourceV0::new(path, lineage))
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let inventory = LingmaSourceInventoryV0::new(authority_key, databases)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(lingma_inventory_registration(
        source,
        selection,
        data_root,
        Arc::new(FixedLingmaInventorySource { inventory }),
    ))
}

trait LingmaInventorySource: Send + Sync {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>;
}

#[derive(Debug, Clone)]
struct FixedLingmaInventorySource {
    inventory: LingmaSourceInventoryV0,
}

impl LingmaInventorySource for FixedLingmaInventorySource {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        Ok(self.inventory.clone())
    }
}

struct DiscoveredLingmaInventorySource<F> {
    observe: F,
}

impl<F> LingmaInventorySource for DiscoveredLingmaInventorySource<F>
where
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync,
{
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        (self.observe)()
            .map_err(lingma_discovery_adapter_error)
            .and_then(lingma_adapter_inventory)
    }
}

fn lingma_inventory_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory_source: Arc<dyn LingmaInventorySource>,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_inventory = Arc::clone(&inventory_source);
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        LingmaInventoryProvider { inventory_source },
    );
    SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
        Some(Box::new(move || {
            let inventory = watch_inventory.observe().ok()?;
            Some(sqlite_inventory_watch_targets(
                inventory
                    .databases()
                    .iter()
                    .map(LingmaDatabaseSourceV0::path),
            ))
        })),
    )
}

pub struct LingmaInventoryProvider {
    inventory_source: Arc<dyn LingmaInventorySource>,
}

impl<L, S> SqliteInventoryProvider<L, S> for LingmaInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = LingmaDatabaseSourceV0;

    fn parser_revision(&self) -> &'static str {
        LINGMA_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = self.inventory_source.observe().map_err(route_error)?;
        reject_duplicate_lingma_paths(&inventory).map_err(route_error)?;
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(inventory.observation())?;
        let leaves = inventory
            .databases()
            .iter()
            .cloned()
            .map(|leaf| {
                Ok(SqliteInventoryCatalogLeaf {
                    source: leaf.source_key()?,
                    physical_locator: leaf.path().to_path_buf(),
                    provider_leaf: leaf,
                })
            })
            .collect::<LingmaSourceBackedResultV0<Vec<_>>>()
            .map_err(route_error)?;
        Ok(SqliteInventoryCatalog {
            authority_fingerprint,
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut sink_failure = None;
        let certificate = scan_lingma_snapshot_v0(leaf, snapshot, &mut |record| {
            if let Err(error) = sink.emit_core_record(record) {
                let detail = error.to_string();
                sink_failure = Some(error);
                return Err(LingmaSourceBackedErrorV0::Capture(
                    CaptureError::InvalidPayload(detail),
                ));
            }
            Ok(())
        });
        lingma_scan_route_result(sink_failure, certificate)
    }
}

fn lingma_scan_route_result(
    sink_failure: Option<SourceBackedRouteError>,
    certificate: LingmaSourceBackedResultV0<CertifiedSource>,
) -> SourceBackedRouteResult<CertifiedSource> {
    if let Some(primary) = sink_failure {
        if let Err(LingmaSourceBackedErrorV0::SnapshotCleanup { cleanup, .. }) = certificate {
            return Err(combine_primary_and_cleanup_route_errors(
                primary,
                sqlite_source_route_error(cleanup),
            ));
        }
        return Err(primary);
    }
    certificate.map_err(lingma_inventory_route_error)
}

fn shelley_inventory_route_error(
    error: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError,
) -> SourceBackedRouteError {
    use crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError;
    if let ShelleySourceBackedError::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            shelley_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
    let kind = match &error {
        ShelleySourceBackedError::SqliteSource(error) => sqlite_source_route_error_kind(error),
        ShelleySourceBackedError::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn abort_shelley_inventory_scan(
    scan: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedScan,
    primary: SourceBackedRouteError,
) -> SourceBackedRouteError {
    match scan.abort() {
        Ok(()) => primary,
        Err(cleanup) => {
            combine_primary_and_cleanup_route_errors(primary, sqlite_source_route_error(cleanup))
        }
    }
}

fn lingma_inventory_route_error(error: LingmaSourceBackedErrorV0) -> SourceBackedRouteError {
    if let LingmaSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            lingma_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
    let kind = match &error {
        LingmaSourceBackedErrorV0::SqliteSource(error) => sqlite_source_route_error_kind(error),
        LingmaSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LingmaRegistrationError {
    SelectorAuthorityUnavailable(&'static str),
    RegistrationRejected(String),
}

pub fn discovered_lingma_registration<L, S, F>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    observe: F,
) -> std::result::Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
    LingmaRegistrationError,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync
        + 'static,
{
    let inventory = discovered_lingma_inventory_source(&source, observe)?;
    Ok(lingma_inventory_registration(
        source, selection, data_root, inventory,
    ))
}

fn discovered_lingma_inventory_source<F>(
    selected_source: &ProviderSource,
    observe: F,
) -> std::result::Result<Arc<dyn LingmaInventorySource>, LingmaRegistrationError>
where
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync
        + 'static,
{
    let source = DiscoveredLingmaInventorySource { observe };
    let opening = (source.observe)()
        .map_err(|error| LingmaRegistrationError::SelectorAuthorityUnavailable(error.detail()))?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.source() == selected_source)
    {
        return Err(LingmaRegistrationError::SelectorAuthorityUnavailable(
            "Lingma selected database is absent from its installed-client inventory",
        ));
    }
    lingma_adapter_inventory(opening)
        .map_err(|error| LingmaRegistrationError::RegistrationRejected(error.to_string()))?;
    Ok(Arc::new(source))
}

pub(crate) fn sqlite_source_route_error(
    error: crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(sqlite_source_route_error_kind(&error), error.to_string())
}

pub(crate) fn sqlite_source_route_error_kind(
    error: &crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteErrorKind {
    if error.is_source_changed() {
        SourceBackedRouteErrorKind::SourceChanged
    } else if error.is_systemic_resource_failure() || error.is_busy_or_locked() {
        SourceBackedRouteErrorKind::ResourceUnavailable
    } else if error.is_ctx_owned_corruption() {
        SourceBackedRouteErrorKind::Internal
    } else if error.is_provider_corruption() || error.is_provider_path_unavailable() {
        SourceBackedRouteErrorKind::InvalidSource
    } else if error.is_operational_failure() {
        SourceBackedRouteErrorKind::Internal
    } else {
        SourceBackedRouteErrorKind::InvalidSource
    }
}

pub(crate) fn sqlite_capture_route_error(
    error: &CaptureError,
) -> Option<SourceBackedRouteErrorKind> {
    match error {
        CaptureError::SourceChangedDuringCapture => Some(SourceBackedRouteErrorKind::SourceChanged),
        CaptureError::Io(error) | CaptureError::SystemIo { source: error, .. }
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Sqlite(error)
            if crate::provider_sources::rusqlite_resource_failure(error)
                || crate::provider_sources::rusqlite_busy_or_locked(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Io(_) | CaptureError::SystemIo { .. } | CaptureError::Sqlite(_) => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        _ => None,
    }
}

fn lingma_adapter_inventory(
    inventory: LingmaDiscoveredInventory,
) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
    let authority_key = inventory
        .authority_key()
        .map_err(lingma_discovery_adapter_error)?;
    let databases = inventory
        .databases()
        .iter()
        .map(|database| {
            let lineage = database
                .catalog_lineage()
                .typed_key()
                .map_err(lingma_discovery_adapter_error)?;
            LingmaDatabaseSourceV0::new(database.path(), lineage)
        })
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()?;
    LingmaSourceInventoryV0::new(authority_key, databases)
}

fn lingma_discovery_adapter_error(error: LingmaDiscoveryUnavailable) -> LingmaSourceBackedErrorV0 {
    CaptureError::InvalidPayload(error.to_string()).into()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

    use ctx_history_capture_model::SourceRouteIdentity;
    use ctx_history_capture_runtime::{
        BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
        CapturePublicationContext, CapturePublicationDisposition, CaptureRevalidationTarget,
        CoreMaterialization, CorePreparationFailureKind, CorePreparationPort,
        ImmutableCaptureSnapshot, PresentCaptureRoute, SourceBackedCertifiedRemoval,
        SourceBackedLogicalSourceFailures, SourceBackedRecordRejections,
        SourceBackedRouteResources,
    };
    use ctx_history_core::{
        CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
        CertifiedSourceInventory, CoreRecord, SourceKey, TypedKey,
    };
    use rusqlite::Connection;
    use uuid::Uuid;

    use super::*;

    #[derive(Clone, Default)]
    pub(crate) struct NoopLookup;

    impl BaseEventLookup for NoopLookup {
        type Error = std::io::Error;

        fn contains(&self, _event_id: Uuid) -> std::result::Result<bool, Self::Error> {
            Ok(false)
        }
    }

    #[derive(Clone, Default)]
    pub(crate) struct NoopPreparation;

    impl CorePreparationPort for NoopPreparation {
        type Prepared = CoreRecord;
        type Draft = CoreRecord;
        type Failure = std::io::Error;

        fn prepare(
            &self,
            record: CoreRecord,
        ) -> std::result::Result<Self::Prepared, Self::Failure> {
            Ok(record)
        }

        fn prepare_draft(
            &self,
            record: CoreRecord,
        ) -> std::result::Result<Self::Draft, Self::Failure> {
            Ok(record)
        }

        fn materialize_draft(
            &self,
            draft: Self::Draft,
            _maximum_encoded_bytes: usize,
        ) -> std::result::Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure>
        {
            Ok(CoreMaterialization::Prepared(draft))
        }

        fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
            &prepared.source
        }

        fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
            prepared
                .encode_stored()
                .map(|encoded| encoded.len())
                .unwrap_or(0)
        }

        fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
            CorePreparationFailureKind::Internal
        }
    }

    #[derive(Clone, Default)]
    pub(crate) struct NoopSnapshot;

    impl ImmutableCaptureSnapshot for NoopSnapshot {
        fn sources(&self) -> &[CertifiedSource] {
            &[]
        }

        fn source_aggregates(
            &self,
        ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureSourceAggregateRef<'_>>
        {
            std::iter::empty()
        }

        fn source_routes(
            &self,
        ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureRouteRef<'_>>
        {
            std::iter::empty()
        }

        fn source_route(
            &self,
            _route_identity: &SourceRouteIdentity,
        ) -> Option<ctx_history_capture_runtime::CaptureRouteRef<'_>> {
            None
        }
    }

    #[derive(Default)]
    pub(crate) struct NoopLifecycle;

    impl CaptureLifecycleSink for NoopLifecycle {
        type Error = std::io::Error;
        type OpenOptions = ();
        type BaseLookup = NoopLookup;
        type Preparation = NoopPreparation;
        type PinnedAppendBase = CertifiedSource;
        type CommittedSnapshot = NoopSnapshot;
        type VerifiedPublication = ();
        type Snapshot<'a> = NoopSnapshot;

        fn invariant_error(detail: &'static str) -> Self::Error {
            std::io::Error::other(detail)
        }

        fn open(
            _root: &std::path::Path,
            _options: Self::OpenOptions,
        ) -> std::result::Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
            Ok(CaptureLifecycleOpenOutcome::Ready(Self))
        }

        fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
            None
        }

        fn base_source(&self, _source: &SourceKey) -> Option<&CertifiedSource> {
            None
        }

        fn pinned_append_base(
            &self,
            _route_identity: &SourceRouteIdentity,
            _source: &SourceKey,
        ) -> Option<Self::PinnedAppendBase> {
            None
        }

        fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
            base
        }

        fn base_event_lookup(&self) -> Self::BaseLookup {
            NoopLookup
        }

        fn core_preparation(&self) -> Self::Preparation {
            NoopPreparation
        }

        fn set_route_plan(
            &mut self,
            _selected: BTreeSet<SourceRouteIdentity>,
            _carried_from_base: BTreeSet<SourceRouteIdentity>,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn begin_route_stage(
            &mut self,
            _route_identity: SourceRouteIdentity,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn retain_unstaged_route_members(
            &mut self,
            _route_identity: &SourceRouteIdentity,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
            false
        }

        fn register_route_revalidation(
            &mut self,
            _route_identity: SourceRouteIdentity,
            _revalidate: impl Fn() -> bool + Send + 'static,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn visit_revalidation_targets<E>(
            &self,
            _visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> std::result::Result<(), E>,
        ) -> std::result::Result<std::result::Result<(), E>, Self::Error> {
            Ok(Ok(()))
        }

        fn finish_route_stage(
            &mut self,
            _route_identity: &SourceRouteIdentity,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn rollback_route_stage(
            &mut self,
            _route_identity: &SourceRouteIdentity,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn authorize_carried_route_retirement(
            &mut self,
            _replacement_route: &SourceRouteIdentity,
            _retired_route: &SourceRouteIdentity,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn retire_carried_route(
            &mut self,
            _replacement_route: &SourceRouteIdentity,
            _retired_route: &SourceRouteIdentity,
        ) -> std::result::Result<Vec<SourceKey>, Self::Error> {
            Ok(Vec::new())
        }

        fn begin_source_replace(
            &mut self,
            _source: SourceKey,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn begin_source_append(
            &mut self,
            _source: SourceKey,
        ) -> std::result::Result<&CertifiedSource, Self::Error> {
            Err(std::io::Error::other("no append base"))
        }

        fn begin_source_append_from_base(
            &mut self,
            base: Self::PinnedAppendBase,
        ) -> std::result::Result<&CertifiedSource, Self::Error> {
            let _ = base;
            Err(std::io::Error::other("no append base"))
        }

        fn add_prepared(
            &mut self,
            _prepared: <Self::Preparation as CorePreparationPort>::Prepared,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn certify_source(
            &mut self,
            _certificate: CertifiedSource,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn certify_source_append(
            &mut self,
            _append: CertifiedSourceAppend,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn retain_source(
            &mut self,
            _certificate: CertifiedSource,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn certify_complete_inventory(
            &mut self,
            _inventory: CertifiedSourceInventory,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn delete_source(
            &mut self,
            _deletion: CertifiedSourceDeletion,
            _inventory: CertifiedSourceInventory,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn carry_failed_route(
            &mut self,
            _route_identity: &SourceRouteIdentity,
        ) -> std::result::Result<bool, Self::Error> {
            Ok(false)
        }

        fn observe_missing_route(
            &mut self,
            _route_identity: SourceRouteIdentity,
            _observed_at_unix_ms: u64,
            _revalidate_missing: impl Fn() -> bool + Send + 'static,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn set_present_routes(
            &mut self,
            _routes: impl IntoIterator<Item = PresentCaptureRoute>,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn commit<F, I>(
            self,
            _revalidate: F,
            _revalidate_inventory: I,
        ) -> std::result::Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
        where
            F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
            I: FnMut(&CertifiedSourceInventory) -> bool,
        {
            Ok(CaptureCommitReceipt::new(
                "noop-generation".to_owned(),
                1,
                0,
                0,
                0,
                NoopSnapshot,
            ))
        }

        fn commit_with_metadata<F, I, M>(
            self,
            _revalidate: F,
            _revalidate_inventory: I,
            metadata_factory: M,
        ) -> std::result::Result<
            CaptureCommitOutcome<Self::CommittedSnapshot, Self::VerifiedPublication>,
            Self::Error,
        >
        where
            F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
            I: FnMut(&CertifiedSourceInventory) -> bool,
            M: for<'a> FnOnce(
                CapturePublicationContext<'a, Self::Snapshot<'a>>,
            ) -> std::result::Result<Vec<u8>, Self::Error>,
        {
            let snapshot = NoopSnapshot;
            let _ = metadata_factory(CapturePublicationContext::new(
                "noop-generation",
                snapshot.clone(),
            ))?;
            Ok(CaptureCommitOutcome::new(
                CaptureCommitReceipt::new("noop-generation".to_owned(), 1, 0, 0, 0, snapshot),
                CapturePublicationDisposition::Published,
                ctx_history_capture_runtime::VerifiedCapture::new(()),
            ))
        }
    }

    #[derive(Default)]
    struct NoopSpool(Vec<CoreRecord>);

    impl DocumentRecordSpool for NoopSpool {
        fn new(_resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
            Ok(Self::default())
        }

        fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
            self.0.push(record);
            Ok(())
        }

        fn replay(
            self,
            mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
        ) -> SourceBackedRouteResult<()> {
            for record in self.0 {
                emit(record)?;
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct TestCrushInventory {
        observation: CrushProjectInventoryObservationV0,
    }

    impl CrushProjectInventorySourceV0 for TestCrushInventory {
        fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
            Ok(self.observation.clone())
        }

        fn record_projection_pass(&self) {}

        fn record_snapshot_work(
            &self,
            _work: crate::provider::providers::crush::native_path::source_backed::CrushSnapshotWorkV0,
        ) {
        }
    }

    fn create_astrbot_database(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "pragma user_version = 4;
                 create table conversations (
                     id integer primary key,
                     inner_conversation_id text,
                     conversation_id text,
                     platform_id text,
                     user_id text,
                     content text not null,
                     title text,
                     persona_id text,
                     token_usage text,
                     created_at integer,
                     updated_at integer
                 );
                 create table platform_message_history (
                     id integer primary key,
                     platform_id text,
                     user_id text,
                     sender_id text,
                     sender_name text,
                     content text,
                     llm_checkpoint_id text,
                     created_at integer
                 );
                 insert into conversations (
                     id, inner_conversation_id, conversation_id, platform_id, user_id, content,
                     title, persona_id, token_usage, created_at, updated_at
                 ) values (
                     1, 'session', 'conversation', 'webchat', 'user',
                     '[{\"id\":\"message\",\"role\":\"user\",\"content\":\"body\"}]',
                     'title', 'persona', '{\"prompt\":1,\"completion\":2}', 1, 1
                 );",
            )
            .unwrap();
    }

    fn fixture_provider_source(
        provider: CaptureProvider,
        path: PathBuf,
        source_format: &'static str,
    ) -> ProviderSource {
        ProviderSource {
            provider,
            path,
            exists: true,
            source_format,
            source_kind: ctx_history_capture_model::ProviderSourceKind::NativeHistory,
            import_support: ctx_history_capture_model::ProviderImportSupport::Native,
            catalog_support: ctx_history_capture_model::ProviderCatalogSupport::None,
            status: crate::ProviderSourceStatus::Available,
            unsupported_reason: None,
        }
    }

    #[test]
    fn sqlite_inventory_watch_targets_include_databases_and_authority_parents() {
        let first = PathBuf::from("/tmp/a/history.sqlite");
        let second = PathBuf::from("/tmp/b/state.db");
        let targets = sqlite_inventory_watch_targets([first.as_path(), second.as_path()]);
        assert_eq!(targets.sqlite_databases.len(), 2);
        assert!(targets.sqlite_databases.contains(&first));
        assert!(targets.sqlite_databases.contains(&second));
        assert!(targets.authority_paths.contains(&PathBuf::from("/tmp/a")));
        assert!(targets.authority_paths.contains(&PathBuf::from("/tmp/b")));
    }

    #[test]
    fn astrbot_registration_watch_targets_cover_selected_and_launcher_instances() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("workspace");
        let selected = cwd.join("data/data_v4.db");
        let launcher = home.join(
            ".astrbot_launcher/instances/123e4567-e89b-12d3-a456-426614174000/core/data/data_v4.db",
        );
        let ignored = home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db");
        create_astrbot_database(&selected);
        create_astrbot_database(&launcher);
        create_astrbot_database(&ignored);

        let registration = astrbot_registration::<NoopLifecycle, NoopSpool>(
            fixture_provider_source(
                CaptureProvider::AstrBot,
                selected.clone(),
                ASTRBOT_SQLITE_SOURCE_FORMAT,
            ),
            SourceBackedRouteSelection::Automatic,
            crate::test_provider_sqlite_data_root(),
            DiscoveryContext::new(
                &home,
                &cwd,
                ctx_history_source_discovery::DiscoveryPlatform::Linux,
                ctx_history_source_discovery::DiscoveryPlatformDirs::default(),
            ),
        );
        let (_, _, _, _, watch_targets) = registration.into_parts();
        let targets = watch_targets.unwrap()().unwrap();
        assert!(targets.sqlite_databases.contains(&selected));
        assert!(targets.sqlite_databases.contains(&launcher));
        assert!(!targets.sqlite_databases.contains(&ignored));
        assert!(targets.authority_paths.contains(selected.parent().unwrap()));
        assert!(targets.authority_paths.contains(launcher.parent().unwrap()));
        assert!(targets
            .authority_paths
            .contains(&home.join(".astrbot_launcher").join("instances")));
    }

    #[test]
    fn crush_registration_watch_targets_follow_observed_inventory() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = temp.path().join("alpha/project.db");
        let second = temp.path().join("beta/project.db");
        let observation = CrushProjectInventoryObservationV0::new(
            TypedKey::utf8("crush-inventory").unwrap(),
            b"revision".to_vec(),
            vec![
                CrushProjectDatabaseV0::new(TypedKey::utf8("alpha").unwrap(), first.clone())
                    .unwrap(),
                CrushProjectDatabaseV0::new(TypedKey::utf8("beta").unwrap(), second.clone())
                    .unwrap(),
            ],
        )
        .unwrap();
        let registration = crush_registration::<TestCrushInventory, NoopLifecycle, NoopSpool>(
            fixture_provider_source(
                CaptureProvider::Crush,
                first.clone(),
                CRUSH_SQLITE_SOURCE_FORMAT,
            ),
            SourceBackedRouteSelection::Automatic,
            crate::test_provider_sqlite_data_root(),
            Arc::new(TestCrushInventory { observation }),
        );
        let (_, _, _, _, watch_targets) = registration.into_parts();
        let targets = watch_targets.unwrap()().unwrap();
        assert!(targets.sqlite_databases.contains(&first));
        assert!(targets.sqlite_databases.contains(&second));
        assert!(targets.authority_paths.contains(first.parent().unwrap()));
        assert!(targets.authority_paths.contains(second.parent().unwrap()));
    }

    #[test]
    fn lingma_registration_watch_targets_follow_inventory_databases() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = temp.path().join("alpha/state.vscdb");
        let second = temp.path().join("beta/state.vscdb");
        let registration = lingma_registration::<NoopLifecycle, NoopSpool>(
            fixture_provider_source(
                CaptureProvider::Lingma,
                first.clone(),
                LINGMA_SQLITE_SOURCE_FORMAT,
            ),
            SourceBackedRouteSelection::Automatic,
            crate::test_provider_sqlite_data_root(),
            TypedKey::utf8("lingma-authority").unwrap(),
            vec![
                (first.clone(), TypedKey::utf8("lineage-alpha").unwrap()),
                (second.clone(), TypedKey::utf8("lineage-beta").unwrap()),
            ],
        )
        .unwrap();
        let (_, _, _, _, watch_targets) = registration.into_parts();
        let targets = watch_targets.unwrap()().unwrap();
        assert!(targets.sqlite_databases.contains(&first));
        assert!(targets.sqlite_databases.contains(&second));
        assert!(targets.authority_paths.contains(first.parent().unwrap()));
        assert!(targets.authority_paths.contains(second.parent().unwrap()));
    }
}
