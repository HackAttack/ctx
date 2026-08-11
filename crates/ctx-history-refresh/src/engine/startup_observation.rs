use super::*;

pub(super) fn overdue_hermes_exact_routes(
    index: &VerifiedIndex,
    now_ms: i64,
) -> BTreeSet<SourceRouteIdentity> {
    let manifest = index.manifest();
    manifest
        .source_routes()
        .iter()
        .filter_map(|route| {
            let route_sources = route
                .sources()
                .iter()
                .map(|source| source.identity().digest())
                .collect::<BTreeSet<_>>();
            let certificates = manifest
                .sources
                .iter()
                .filter(|source| {
                    route_sources.contains(&source.observation().source().identity().digest())
                })
                .cloned()
                .collect::<Vec<_>>();
            ctx_history_capture::hermes_sources_require_exact_reconciliation(&certificates, now_ms)
                .then(|| route.route_identity().clone())
        })
        .collect()
}

pub(super) fn startup_routes_requiring_refresh(
    catalog: &SourceBackedWatchCatalog,
    expected: Option<&BTreeMap<SourceRouteIdentity, String>>,
    missing_routes: &BTreeSet<SourceRouteIdentity>,
    budget: StdDuration,
) -> Vec<SourceRouteIdentity> {
    let started = StdInstant::now();
    catalog
        .route_ids()
        .filter(|route| {
            let Some(expected) = expected else {
                return true;
            };
            if started.elapsed() >= budget || missing_routes.contains(*route) {
                return true;
            }
            !matches!(
                catalog.observe_route(route, expected.get(*route).map(String::as_str)),
                RouteObservation::Unchanged
            )
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_capture::{
        SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    };
    use ctx_history_capture_model::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus,
    };
    use ctx_history_core::{
        CertifiedSource, ScannedSourceCounts, SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, SourceRouteSnapshot, VerifiedIndex, WriterOptions};

    fn hermes_control_index(
        root: &Path,
        route: &SourceRouteIdentity,
        exact_due_at_ms: i64,
    ) -> VerifiedIndex {
        let source = SourceKey::derive_provider_native(
            CaptureProvider::Hermes.as_str(),
            "hermes_state_sqlite",
            "hermes-state-control-v1",
            1,
            "hermes-test-control",
            TypedKey::U64(1),
        )
        .unwrap();
        let database_identity = [1_u8; 32];
        let schema_evidence = [2_u8; 32];
        let revision = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "database_identity": database_identity,
            "schema_evidence": schema_evidence,
            "session_rowid": 4,
            "message_rowid": 9,
            "last_successful_exhaustive_at_ms": 100,
            "exact_due_at_ms": exact_due_at_ms,
            "exhaustive_sequence": 1,
            "mode": "exhaustive",
            "outcome": "successful",
        }))
        .unwrap();
        let observation =
            SourceObservation::new(source.clone(), "hermes-refresh-control-v1", revision).unwrap();
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            "hermes-source-backed-v3",
            [3; 32],
            ScannedSourceCounts::default(),
        )
        .unwrap();
        let mut writer = GenerationWriter::open(root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.certify_source(certificate).unwrap();
        writer
            .set_present_source_routes(vec![SourceRouteSnapshot::present(
                route.clone(),
                vec![source],
            )
            .unwrap()])
            .unwrap();
        writer.commit(|_| true).unwrap();
        VerifiedIndex::open(root).unwrap()
    }

    #[test]
    fn persisted_hermes_deadline_selects_only_overdue_exact_routes() {
        let temp = tempfile::tempdir().unwrap();
        let route = SourceRouteIdentity::from_sha256("a7".repeat(32)).unwrap();
        let future = hermes_control_index(&temp.path().join("future"), &route, 1_001);
        assert!(overdue_hermes_exact_routes(&future, 1_000).is_empty());
        let overdue = hermes_control_index(&temp.path().join("overdue"), &route, 1_000);
        assert_eq!(
            overdue_hermes_exact_routes(&overdue, 1_000),
            BTreeSet::from([route])
        );
    }

    fn watch_catalog(path: PathBuf) -> (SourceBackedWatchCatalog, SourceRouteIdentity) {
        let source = ProviderSource {
            provider: CaptureProvider::Codex,
            exists: true,
            path,
            source_format: "codex_history_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        };
        let route = SourceBackedRoute::automatic(
            source,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap();
        let identity = route.metadata().route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        (registry.watch_catalog(), identity)
    }

    #[test]
    fn warm_exact_noop_schedules_zero_parser_or_writer_work() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path);
        let expected = BTreeMap::from([(
            route.clone(),
            catalog.certify_route_observation(&route).unwrap(),
        )]);

        assert!(startup_routes_requiring_refresh(
            &catalog,
            Some(&expected),
            &BTreeSet::new(),
            StdDuration::from_secs(1),
        )
        .is_empty());
    }

    #[test]
    fn changed_unavailable_indeterminate_and_budget_expiry_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path.clone());
        let token = catalog.certify_route_observation(&route).unwrap();
        let expected = BTreeMap::from([(route.clone(), token)]);

        fs::write(&path, b"one\ntwo\n").unwrap();
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                None,
                &BTreeSet::new(),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::new(),
                StdDuration::ZERO,
            ),
            vec![route]
        );
    }

    #[test]
    fn missing_grace_never_skips_and_watcher_race_reenters_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let (catalog, route) = watch_catalog(path);
        let expected = BTreeMap::from([(
            route.clone(),
            catalog.certify_route_observation(&route).unwrap(),
        )]);
        assert_eq!(
            startup_routes_requiring_refresh(
                &catalog,
                Some(&expected),
                &BTreeSet::from([route.clone()]),
                StdDuration::from_secs(1),
            ),
            vec![route.clone()]
        );

        let engine = test_refresh_engine();
        engine.initialize_watch_route_authority([route.clone()]);
        assert!(startup_routes_requiring_refresh(
            &catalog,
            Some(&expected),
            &BTreeSet::new(),
            StdDuration::from_secs(1),
        )
        .is_empty());
        engine.record_watch_routes(
            [(route.clone(), EventWatermark::new(7, 1))],
            source_route_ledger_now_ms(),
        );
        assert_eq!(
            engine.scheduled_route_ids_for_test(),
            BTreeSet::from([route])
        );
    }
}
