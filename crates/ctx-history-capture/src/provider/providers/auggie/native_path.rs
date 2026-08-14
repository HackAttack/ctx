use chrono::{DateTime, Utc};

pub(crate) use ctx_history_provider_docproj::providers::auggie::native_path::source_backed;

use crate::provider::source_backed::{
    family::document::register_replacement_document_tree_route, CaptureProviderRuntime,
    SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use crate::{ProviderAdapterContext, ProviderSource};

pub(crate) fn register_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-auggie".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = source_backed::AuggieDocumentTreeAdapter::<CaptureProviderRuntime>::new(
        source_backed::AuggieSourceBackedRoot::explicit(source.path.clone()),
        context,
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}
