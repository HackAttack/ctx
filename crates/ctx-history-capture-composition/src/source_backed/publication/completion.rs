use super::*;

pub(super) fn run_after_successful_publication(
    registry: &SourceBackedProviderRegistry,
    successful_route_ids: &BTreeSet<SourceRouteIdentity>,
) {
    for route in &registry.routes {
        if route
            .metadata
            .route_identity
            .as_ref()
            .is_some_and(|identity| successful_route_ids.contains(identity))
        {
            if let Some(after_publication) = route
                .driver
                .as_ref()
                .and_then(|driver| driver.after_successful_publication.as_ref())
            {
                after_publication();
            }
        }
    }
}
