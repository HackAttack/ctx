//! Capture façade for the provider-neutral document lifecycle runtime.
//!
//! Route registration, stable route-key derivation, concrete staging storage,
//! and the index lifecycle binding remain capture-owned.

use crate::ProviderSource;
use ctx_history_capture_runtime::{
    ChangedDocumentSink as RuntimeChangedDocumentSink,
    DocumentAppendBase as RuntimeDocumentAppendBase, DocumentBaseRoute as RuntimeDocumentBaseRoute,
};
use ctx_history_provider_runtime::ProviderReplacementDocumentTree;

use crate::provider::source_backed::{
    IndexCaptureLifecycle, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
    SourceBackedRouteControlExpectation, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
#[cfg(test)]
use crate::provider::source_backed::{SourceBackedRoute, SourceBackedWatchTargetKind};

mod spool;
use super::CaptureProviderRuntime;

pub type CaptureDocumentSpool = spool::DeferredCoreRecords;
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
    ctx_history_provider_runtime::register_replacement_document_tree_route::<
        CaptureProviderRuntime,
        _,
        _,
    >(registry, source, selection, selector_authority, adapter)
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
    let driver = ctx_history_provider_runtime::provider_replacement_document_tree_driver::<
        CaptureProviderRuntime,
        _,
    >(&source, adapter);
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
    ProviderReplacementDocumentTree<CaptureProviderRuntime>
{
}

impl<A> CaptureReplacementDocumentTree for A where
    A: ProviderReplacementDocumentTree<CaptureProviderRuntime>
{
}
