use super::*;
use std::fs;

use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, SourceBackedRoute,
    SourceBackedRouteDriver,
};

struct UnusedPublishedState;

impl PublishedSourceBackedStatePort for UnusedPublishedState {
    fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
        unreachable!("catalog admission does not open published state")
    }
}

#[test]
fn exhaustive_exact_route_reuses_catalog_without_claiming_member_work() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let database = home.join("history.db");
    fs::write(&database, b"sqlite").unwrap();
    let source = provider_source_for_path(CaptureProvider::OpenCode, database);
    let route = SourceBackedRoute::automatic(
        source.clone(),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "route-local-exhaustive",
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::exact([route_identity]),
        BTreeSet::new(),
        SourceBackedRefreshCoveredPublication::default(),
        &discovery,
        &UnusedPublishedState,
        &progress,
    )
    .with_reconciliation_demand(SourceBackedReconciliationDemand::Exhaustive)
    .with_watch_catalog_opt(Some(registry.watch_catalog()));

    let admission = catalog_refresh_admission(&execution)
        .expect("exact registered route should avoid global discovery");
    assert!(!admission.exact_members);
    assert_eq!(admission.report.sources, vec![source]);
}

#[test]
fn catalog_selector_distinguishes_exact_members_from_global_fallbacks() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    let root = home.join("claude-projects");
    let member = root.join("project/session.jsonl");
    fs::create_dir_all(member.parent().unwrap()).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(&member, b"{}\n").unwrap();
    let source = provider_source_for_path(CaptureProvider::Claude, root.clone());
    let route = SourceBackedRoute::automatic(
        source.clone(),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "exact-member",
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::exact([route_identity.clone()]),
        BTreeSet::new(),
        SourceBackedRefreshCoveredPublication::default(),
        &discovery,
        &UnusedPublishedState,
        &progress,
    )
    .with_route_worksets(BTreeMap::from([(
        route_identity.clone(),
        SourceBackedRefreshWorkset::members([member.clone()]),
    )]))
    .with_watch_catalog_opt(Some(registry.watch_catalog()));

    let exact = catalog_refresh_admission(&execution)
        .expect("valid registered member should stay route-local");
    assert!(exact.exact_members);
    assert_eq!(exact.report.sources, vec![source]);

    let mut invalid_member = execution.clone();
    invalid_member.route_worksets = BTreeMap::from([(
        route_identity.clone(),
        SourceBackedRefreshWorkset::members([root.join("missing.jsonl")]),
    )]);
    let invalid_member = catalog_refresh_admission(&invalid_member)
        .expect("invalid member should retain route-local exhaustive work");
    assert!(!invalid_member.exact_members);

    let mut all = execution.clone();
    all.scope = SourceBackedRefreshScope::All;
    assert!(catalog_refresh_admission(&all).is_none());

    let mut unknown = execution.clone();
    unknown.scope =
        SourceBackedRefreshScope::exact([
            SourceRouteIdentity::from_sha256("ef".repeat(32)).unwrap()
        ]);
    assert!(catalog_refresh_admission(&unknown).is_none());

    fs::remove_dir_all(root).unwrap();
    assert!(catalog_refresh_admission(&execution).is_none());
}

#[test]
fn complete_inventory_member_fallback_runs_only_once() {
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let complete = BTreeSet::from([route.clone()]);
    let members = BTreeMap::from([(route, BTreeSet::from([PathBuf::from("changed.json")]))]);

    assert!(exact_member_family_fallback_required(
        &members,
        &complete,
        &[],
        &[],
    ));
    assert!(!exact_member_family_fallback_required(
        &BTreeMap::new(),
        &complete,
        &[],
        &[],
    ));
}
