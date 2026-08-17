use super::*;

#[derive(Debug)]
pub(super) struct ExactMemberFallbackRequired;

impl std::fmt::Display for ExactMemberFallbackRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("exact member requires registered-route family reconciliation")
    }
}

impl std::error::Error for ExactMemberFallbackRequired {}

pub(super) fn exact_member_family_fallback_required(
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    complete_inventory_routes: &BTreeSet<SourceRouteIdentity>,
    successful_routes: &[SourceBackedSuccessfulRouteOutcome],
    failed_routes: &[SourceBackedFailedRouteOutcome],
) -> bool {
    let exact_routes = route_worksets.keys().collect::<BTreeSet<_>>();
    complete_inventory_routes
        .iter()
        .any(|route| exact_routes.contains(route))
        || successful_routes.iter().any(|outcome| {
            exact_routes.contains(&outcome.route_identity)
                && outcome.logical_source_failure_total != 0
        })
        || failed_routes
            .iter()
            .any(|outcome| exact_routes.contains(&outcome.route_identity))
}
