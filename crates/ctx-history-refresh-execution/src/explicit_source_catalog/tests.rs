#[cfg(test)]
mod tests {
    use super::*;

    fn custom_source(path: PathBuf) -> ProviderSource {
        custom_provider_source(path, true).unwrap()
    }

    #[test]
    fn exact_source_registration_is_an_inline_request_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();

        let request = upsert_explicit_source(&data_root, &custom_source(path.clone())).unwrap();

        assert_eq!(request.path, path);
        assert_eq!(request.authority.route_lineages().len(), 1);
        assert_eq!(
            ExplicitSourceCatalogAuthority::from_json(&request.authority.to_json()).unwrap(),
            request.authority
        );
        assert!(!data_root.join("catalogs/explicit-sources").exists());
    }

    #[test]
    fn request_lineage_is_stable_per_exact_path_and_distinct_across_paths() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let first = temp.path().join("first.jsonl");
        let second = temp.path().join("second.jsonl");
        fs::write(&first, b"\n").unwrap();
        fs::write(&second, b"\n").unwrap();

        let first_request =
            upsert_explicit_source(&data_root, &custom_source(first.clone())).unwrap();
        let repeated = upsert_explicit_source(&data_root, &custom_source(first)).unwrap();
        let second_request = upsert_explicit_source(&data_root, &custom_source(second)).unwrap();

        assert_eq!(first_request.catalog_lineage, repeated.catalog_lineage);
        assert_ne!(
            first_request.catalog_lineage,
            second_request.catalog_lineage
        );
    }

    #[test]
    fn retained_shadow_deduplication_uses_exact_route_keys_not_lineage() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let old_path = temp.path().join("old.jsonl");
        let new_path = temp.path().join("new.jsonl");
        fs::write(&old_path, b"\n").unwrap();
        fs::write(&new_path, b"\n").unwrap();

        let retained = upsert_explicit_source(&data_root, &custom_source(old_path.clone()))
            .unwrap()
            .authority;
        let mut relocated = upsert_explicit_source(&data_root, &custom_source(new_path.clone()))
            .unwrap()
            .authority;
        relocated.entries[0].catalog_lineage = retained.entries[0].catalog_lineage.clone();
        relocated = authority_for(relocated.revision, &relocated.entries).unwrap();

        let mut report = DiscoveryReport {
            sources: vec![
                custom_source(old_path.clone()),
                custom_source(new_path.clone()),
            ],
            issues: Vec::new(),
        };
        retained
            .prepare_retained_discovery_report(Some(&relocated), &mut report)
            .unwrap();
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].path, new_path);

        relocated.prepare_discovery_report(&mut report, &[]);
        assert!(report.sources.is_empty());

        let mut repeated = DiscoveryReport {
            sources: vec![custom_source(old_path.clone())],
            issues: Vec::new(),
        };
        retained
            .prepare_retained_discovery_report(Some(&retained), &mut repeated)
            .unwrap();
        assert_eq!(repeated.sources.len(), 1);
        assert_eq!(repeated.sources[0].path, old_path);
        retained.prepare_discovery_report(&mut repeated, &[]);
        assert!(repeated.sources.is_empty());
    }

    #[test]
    fn exact_automatic_route_can_reclaim_a_retained_explicit_owner() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let sessions = temp.path().join("codex-sessions");
        fs::create_dir_all(&sessions).unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, sessions);
        let retained = upsert_explicit_source(&data_root, &source)
            .unwrap()
            .authority;
        let mut report = DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        };

        retained
            .prepare_retained_discovery_report_with_automatic_routes(
                None,
                &mut report,
                std::slice::from_ref(&source),
            )
            .unwrap();

        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].provider, CaptureProvider::Codex);
    }

    #[test]
    fn grouped_automatic_route_reclaims_its_secondary_registration_root_only() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let codex_root = temp.path().join(".codex");
        let sessions = codex_root.join("sessions");
        let archived_sessions = codex_root.join("archived_sessions");
        let unrelated_path = temp.path().join("unrelated.jsonl");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&archived_sessions).unwrap();
        fs::write(&unrelated_path, b"\n").unwrap();

        let sessions_source = provider_source_for_path(CaptureProvider::Codex, sessions);
        let archived_source =
            provider_source_for_path(CaptureProvider::Codex, archived_sessions.clone());
        let discovery = ctx_history_capture::DiscoveryContext::new(
            temp.path(),
            temp.path(),
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        );
        let build = ctx_history_capture::build_automatic_source_backed_registry_from_report(
            &discovery,
            &data_root,
            DiscoveryReport {
                sources: vec![sessions_source, archived_source.clone()],
                issues: Vec::new(),
            },
        );
        let automatic_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();

        let archived = upsert_explicit_source(&data_root, &archived_source).unwrap();
        let unrelated =
            upsert_explicit_source(&data_root, &custom_source(unrelated_path)).unwrap();
        let archived_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
        let unrelated_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
        let mut entries = vec![
            archived.authority.entries[0].clone(),
            unrelated.authority.entries[0].clone(),
        ];
        sort_and_validate_entries(&mut entries).unwrap();
        let retained = authority_for(1, &entries).unwrap();
        let bindings = vec![
            ExplicitSourceCatalogRouteBinding {
                catalog_lineage: archived.catalog_lineage_hex(),
                route_identity: archived_route.as_str().to_owned(),
            },
            ExplicitSourceCatalogRouteBinding {
                catalog_lineage: unrelated.catalog_lineage_hex(),
                route_identity: unrelated_route.as_str().to_owned(),
            },
        ];

        let retained_secondary_sources = retained
            .secondary_codex_registration_sources(&build.registry)
            .unwrap();
        assert_eq!(retained_secondary_sources, vec![archived_source.clone()]);
        let mut requested_report = DiscoveryReport {
            sources: vec![
                provider_source_for_path(CaptureProvider::Codex, codex_root.join("sessions")),
                archived_source,
            ],
            issues: Vec::new(),
        };
        retained.prepare_discovery_report(
            &mut requested_report,
            &retained_secondary_sources,
        );
        assert_eq!(
            requested_report
                .sources
                .iter()
                .map(|source| source.path.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([codex_root.join("sessions"), archived_sessions])
        );
        assert_eq!(
            retained
                .automatic_reactivation_retirements(
                    &bindings,
                    &build,
                    &BTreeSet::from([automatic_route.clone()]),
                )
                .unwrap(),
            std::collections::BTreeMap::from([(automatic_route, vec![archived_route])])
        );
    }

    #[test]
    fn request_overlay_cannot_encode_deletion_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();
        let request =
            upsert_explicit_source(&temp.path().join("data"), &custom_source(path)).unwrap();
        let mut entries = request.authority.entries.clone();
        entries[0].enabled = false;
        let error = sort_and_validate_entries(&mut entries).unwrap_err();
        assert!(format!("{error:#}").contains("cannot authorize deletion"));
    }
}
