//! Capture façade for the provider-neutral document lifecycle runtime.
//!
//! Route registration, stable route-key derivation, concrete staging storage,
//! and the index lifecycle binding remain capture-owned.

use crate::ProviderSource;
use ctx_history_capture_runtime::ChangedDocumentSink as RuntimeChangedDocumentSink;
use ctx_history_provider_runtime::ProviderReplacementDocumentTree;

use crate::provider::source_backed::{
    IndexCaptureLifecycle, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
    SourceBackedRouteControlExpectation, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
mod spool;
use super::CaptureProviderRuntime;

pub type CaptureDocumentSpool = spool::DeferredCoreRecords;
pub(crate) type CaptureDocumentLifecycle = IndexCaptureLifecycle;
pub(crate) type CaptureDocumentRouteControl = SourceBackedRouteControlExpectation;

pub(crate) struct CaptureSelectedSqliteBinding;

impl ctx_history_providers_sqlite_selected::SelectedSqliteCaptureBinding
    for CaptureSelectedSqliteBinding
{
    type Lifecycle = CaptureDocumentLifecycle;
    type Spool = CaptureDocumentSpool;
    type RouteControl = CaptureDocumentRouteControl;
}

pub(crate) type ChangedDocumentSink<'sink, 'writer> =
    RuntimeChangedDocumentSink<'sink, 'writer, CaptureDocumentLifecycle, CaptureDocumentSpool>;

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
    ctx_history_provider_runtime::register_replacement_document_tree_route::<
        CaptureProviderRuntime,
        _,
        _,
    >(registry, source, selection, selector_authority, adapter)
}

pub(crate) fn install_sqlite_inventory_registration<A>(
    registry: &mut SourceBackedProviderRegistry,
    registration: ctx_history_providers_sqlite_inventory::registration::SqliteInventoryRegistration<
        A,
    >,
) -> SourceBackedCoordinatorResult<()>
where
    A: CaptureReplacementDocumentTree,
{
    let (source, selection, authority, adapter, watch_targets) = registration.into_parts();
    let watch_source = source.clone();
    register_replacement_document_tree_route_with_authority(
        registry, source, selection, authority, adapter,
    )?;
    if let Some(observe) = watch_targets {
        registry.attach_route_watch_targets(&watch_source, observe)?;
    }
    Ok(())
}

pub(crate) trait CaptureReplacementDocumentTree:
    ProviderReplacementDocumentTree<CaptureProviderRuntime>
{
}

impl<A> CaptureReplacementDocumentTree for A where
    A: ProviderReplacementDocumentTree<CaptureProviderRuntime>
{
}
