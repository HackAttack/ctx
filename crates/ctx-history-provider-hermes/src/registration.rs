use std::path::Path;

use ctx_history_capture_runtime::{
    CaptureLifecycleSink, DocumentRecordSpool, ReplacementDocumentTree, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::SourceAnchor;

use crate::{CaptureError, ProviderSource, Result};

/// Complete Hermes-owned registration contract. Capture consumes this
/// fragment only to bind its concrete lifecycle and install one executable
/// route; provider selection remains fixed here.
pub struct HermesRegistration<A> {
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
}

impl<A> HermesRegistration<A> {
    fn new(
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        selector_authority: SourceBackedSelectorAuthority,
        adapter: A,
    ) -> Self {
        Self {
            source,
            selection,
            selector_authority,
            adapter,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        ProviderSource,
        SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
        A,
    ) {
        (
            self.source,
            self.selection,
            self.selector_authority,
            self.adapter,
        )
    }
}

pub fn hermes_automatic_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> Result<
    HermesRegistration<
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
        crate::provider::source_backed::HermesSourceCandidate::automatic(data_root, source.clone())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(HermesRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        crate::provider::source_backed::replacement::HermesDocumentAdapter::new(candidate),
    ))
}

pub fn hermes_explicit_registration<L, S>(
    source: ProviderSource,
    data_root: &Path,
    anchor: SourceAnchor,
) -> Result<
    HermesRegistration<
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
    let candidate = crate::provider::source_backed::hermes_source_backed_explicit(
        data_root,
        source.path.clone(),
        anchor,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(HermesRegistration::new(
        source,
        SourceBackedRouteSelection::ExplicitManual,
        SourceBackedSelectorAuthority::ExplicitPath,
        crate::provider::source_backed::replacement::HermesDocumentAdapter::new(candidate),
    ))
}

#[cfg(test)]
pub(crate) mod tests;
