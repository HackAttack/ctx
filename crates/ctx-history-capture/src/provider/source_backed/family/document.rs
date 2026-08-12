//! Capture façade for the provider-neutral document lifecycle runtime.
//!
//! Route registration, stable route-key derivation, concrete staging storage,
//! and the index lifecycle binding remain capture-owned.

use sha2::{Digest, Sha256};

use crate::ProviderSource;
use ctx_history_capture_runtime::{
    replacement_document_tree_driver as runtime_document_tree_driver,
    ChangedDocumentSink as RuntimeChangedDocumentSink,
    DocumentAppendBase as RuntimeDocumentAppendBase, DocumentBaseRoute as RuntimeDocumentBaseRoute,
    DocumentInventoryAuthority,
};

use crate::provider::source_backed::{
    executable_route, IndexCaptureLifecycle, SourceBackedCoordinatorResult,
    SourceBackedProviderRegistry, SourceBackedRouteControlExpectation, SourceBackedRouteDriver,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
#[cfg(test)]
use crate::provider::source_backed::{SourceBackedRoute, SourceBackedWatchTargetKind};

mod spool;
pub(crate) type CaptureDocumentSpool = spool::DeferredCoreRecords;
pub(crate) type CaptureDocumentLifecycle = IndexCaptureLifecycle;
pub(crate) type CaptureDocumentRouteControl = SourceBackedRouteControlExpectation;

pub(crate) type ChangedDocumentSink<'sink, 'writer> =
    RuntimeChangedDocumentSink<'sink, 'writer, CaptureDocumentLifecycle, CaptureDocumentSpool>;
pub(crate) type DocumentAppendBase = RuntimeDocumentAppendBase<CaptureDocumentLifecycle>;
pub(crate) type DocumentBaseRoute<'scan, 'writer> =
    RuntimeDocumentBaseRoute<'scan, 'writer, CaptureDocumentLifecycle>;

pub(crate) use ctx_history_capture_runtime::{
    CompleteDocumentTree, DocumentLeafExecutionPolicy, DocumentLeafFingerprint,
    DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
};

pub(crate) fn register_replacement_document_tree_route<A>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    adapter: A,
) -> SourceBackedCoordinatorResult<()>
where
    A: CaptureReplacementDocumentTree,
{
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
    )
}

pub(crate) fn register_replacement_document_tree_route_with_authority<A>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
) -> SourceBackedCoordinatorResult<()>
where
    A: CaptureReplacementDocumentTree,
{
    let driver = replacement_document_tree_driver(&source, adapter);
    registry.register(executable_route(
        source,
        selection,
        selector_authority,
        driver,
    )?);
    Ok(())
}

#[cfg(test)]
pub(crate) fn register_replacement_document_tree_route_unchecked_for_test<A>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selector_authority: SourceBackedSelectorAuthority,
    certified_source_format: &'static str,
    watch_target_kind: SourceBackedWatchTargetKind,
    adapter: A,
) -> SourceBackedCoordinatorResult<()>
where
    A: CaptureReplacementDocumentTree,
{
    let driver = replacement_document_tree_driver(&source, adapter);
    registry.register(SourceBackedRoute::explicit_manual_unchecked_for_test(
        source,
        selector_authority,
        certified_source_format,
        watch_target_kind,
        driver,
    )?);
    Ok(())
}

pub(crate) trait CaptureReplacementDocumentTree:
    ReplacementDocumentTree<
    Lifecycle = CaptureDocumentLifecycle,
    Spool = CaptureDocumentSpool,
    RouteControl = CaptureDocumentRouteControl,
>
{
}

impl<A> CaptureReplacementDocumentTree for A where
    A: ReplacementDocumentTree<
        Lifecycle = CaptureDocumentLifecycle,
        Spool = CaptureDocumentSpool,
        RouteControl = CaptureDocumentRouteControl,
    >
{
}

fn replacement_document_tree_driver<A>(
    route: &ProviderSource,
    adapter: A,
) -> SourceBackedRouteDriver
where
    A: CaptureReplacementDocumentTree,
{
    runtime_document_tree_driver(document_inventory_authority(route), adapter)
}

fn document_inventory_authority(route: &ProviderSource) -> DocumentInventoryAuthority {
    let path = route.path.as_os_str().as_encoded_bytes();
    let mut digest = Sha256::new();
    digest.update(b"ctx.document-tree-route-authority-v1\0");
    digest.update((route.provider.as_str().len() as u64).to_be_bytes());
    digest.update(route.provider.as_str().as_bytes());
    digest.update((route.source_format.len() as u64).to_be_bytes());
    digest.update(route.source_format.as_bytes());
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    DocumentInventoryAuthority::new(route.provider.as_str().to_owned(), digest.finalize().into())
}
