use super::*;

pub(super) fn base_sources_for_root<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    inventory: &JsonlFamilyInventory<JsonlRuntimeError<R>>,
    requested_root: &Path,
    sink: &SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> SourceBackedRouteResult<Vec<CertifiedSource>> {
    let sources: Vec<CertifiedSource> = match adapter.base_scope() {
        JsonlFamilyBaseScope::ProviderFamily => sink
            .lifecycle
            .base_snapshot()
            .map(|snapshot| {
                snapshot
                    .sources()
                    .iter()
                    .filter(|source| adapter.owns(source.observation().source()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        JsonlFamilyBaseScope::Route => sink
            .base_route_sources()
            .map_err(route_internal)?
            .into_values()
            .filter(|source| adapter.owns(source.observation().source()))
            .collect(),
    };
    match adapter.base_scope() {
        JsonlFamilyBaseScope::ProviderFamily => sources
            .into_iter()
            .filter_map(|source| match adapter.base_source_path(&source) {
                Ok(path)
                    if inventory.authorities.is_empty() && path.starts_with(requested_root)
                        || inventory
                            .authorities
                            .iter()
                            .any(|authority| path.starts_with(authority.named_path())) =>
                {
                    Some(Ok(source))
                }
                Ok(_) => None,
                Err(error) => Some(Err(route_invalid(error))),
            })
            .collect(),
        JsonlFamilyBaseScope::Route => Ok(sources),
    }
}
