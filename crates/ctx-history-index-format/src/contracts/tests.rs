use super::*;

#[test]
fn source_route_snapshot_and_generation_wire_contract_remain_stable() {
    let route_identity = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let snapshot = SourceRouteSnapshot::present(route_identity, Vec::new()).unwrap();

    assert_eq!(
        serde_json::to_string(&snapshot).unwrap(),
        format!(
            "{{\"route_identity\":\"{}\",\"sources\":[],\"missing\":null}}",
            "ab".repeat(32)
        )
    );

    let manifest = GenerationManifest::from_parts(Vec::new(), vec![snapshot]).unwrap();
    assert_eq!(
        serde_json::to_string(&manifest).unwrap(),
        "{\"manifest_version\":9,\"identity_version\":1,\"core_record_version\":3,\"core_record_contract_fingerprint\":\"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e\",\"lexical_schema_version\":22,\"lexical_analyzer_version\":2,\"policy_schema_hash\":\"98a522ab684f09534a71628117e182f3559d7094880609a74e81041d00361475\",\"indexed_documents\":0,\"certified_source_bytes\":0,\"sources\":[],\"core_record_aggregates\":[],\"source_routes\":[{\"route_identity\":\"abababababababababababababababababababababababababababababababab\",\"sources\":[],\"missing\":null}],\"automatic_provider_discovery\":true,\"provider_root_config_digest\":\"4bfe780cf41a834d4bd7c58d54498cc96b6a5a1d6b20c37f212af31aaa674064\",\"provider_roots\":[]}",
    );
    assert_eq!(
        manifest.generation_id().unwrap(),
        "fcdf9eff3027899d2ea0c08a898c70157b9b07fc9cb9f1ee9638d7acd6b96861"
    );
}

#[test]
fn provider_root_aliases_are_bounded_and_generation_local() {
    let route_identity = SourceRouteIdentity::from_sha256("cd".repeat(32)).unwrap();
    let definition = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Claude,
        path: std::path::PathBuf::from("/home/example/.claude-personal"),
        scope: Some("personal".to_owned()),
    };
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        Vec::new(),
        Vec::new(),
        vec![SourceRouteSnapshot::present(route_identity.clone(), Vec::new()).unwrap()],
        true,
        provider_source_config_digest(true, std::slice::from_ref(&definition)),
        vec![AppliedProviderRoot::new(definition, vec![route_identity]).unwrap()],
    )
    .unwrap();

    assert_eq!(manifest.provider_roots().len(), 1);
    assert_eq!(
        manifest
            .provider_root_source_tokens(&["personal".to_owned()], &[])
            .unwrap(),
        Vec::<String>::new()
    );
    assert!(matches!(
        manifest.provider_root_source_tokens(&["work".to_owned()], &[]),
        Err(IndexError::UnknownProviderRootSelector(selector)) if selector == "work"
    ));
    assert!(matches!(
        manifest.provider_root_source_tokens(&[], &["work".to_owned()]),
        Err(IndexError::UnknownProviderRootScope(scope)) if scope == "work"
    ));
}

#[test]
fn provider_root_manifest_rejects_dangling_or_shared_routes() {
    let route_identity = SourceRouteIdentity::from_sha256("ef".repeat(32)).unwrap();
    let definition = |id: &str| ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Codex,
        path: std::path::PathBuf::from(format!("/home/example/.codex-{id}")),
        scope: None,
    };
    let first = definition("first");
    assert!(matches!(
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            provider_source_config_digest(true, std::slice::from_ref(&first)),
            vec![AppliedProviderRoot::new(first, vec![route_identity.clone()]).unwrap()],
        ),
        Err(IndexError::ProviderRootRouteNotRetained { .. })
    ));

    let definitions = vec![definition("first"), definition("second")];
    assert!(matches!(
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            Vec::new(),
            Vec::new(),
            vec![SourceRouteSnapshot::present(route_identity.clone(), Vec::new()).unwrap()],
            true,
            provider_source_config_digest(true, &definitions),
            definitions
                .into_iter()
                .map(|root| AppliedProviderRoot::new(root, vec![route_identity.clone()]).unwrap())
                .collect(),
        ),
        Err(IndexError::SourceRouteOwnedByMultipleProviderRoots { .. })
    ));
}

#[test]
fn malformed_deserialized_route_identity_reaches_complete_manifest_validation() {
    let route_identity = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let manifest = GenerationManifest::from_parts(
        Vec::new(),
        vec![SourceRouteSnapshot::present(route_identity, Vec::new()).unwrap()],
    )
    .unwrap();
    let mut persisted = serde_json::to_value(manifest).unwrap();
    persisted["source_routes"][0]["route_identity"] = serde_json::json!("AB".repeat(32));
    let loaded: GenerationManifest = serde_json::from_value(persisted).unwrap();

    assert!(matches!(
        loaded.validate_contract(),
        Err(IndexError::InvalidSourceRouteIdentity)
    ));
}
