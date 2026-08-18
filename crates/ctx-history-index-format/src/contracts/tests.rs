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
        "{\"manifest_version\":8,\"identity_version\":1,\"core_record_version\":3,\"core_record_contract_fingerprint\":\"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e\",\"lexical_schema_version\":22,\"lexical_analyzer_version\":2,\"policy_schema_hash\":\"98a522ab684f09534a71628117e182f3559d7094880609a74e81041d00361475\",\"indexed_documents\":0,\"certified_source_bytes\":0,\"sources\":[],\"core_record_aggregates\":[],\"source_routes\":[{\"route_identity\":\"abababababababababababababababababababababababababababababababab\",\"sources\":[],\"missing\":null}]}",
    );
    assert_eq!(
        manifest.generation_id().unwrap(),
        "7a79f1d9a89f696b273eff2baab00c038847b3dca1b79993a6b150bee4c29ea2"
    );
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
