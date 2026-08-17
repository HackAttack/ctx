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
        "{\"manifest_version\":8,\"identity_version\":1,\"core_record_version\":3,\"core_record_contract_fingerprint\":\"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e\",\"lexical_schema_version\":21,\"lexical_analyzer_version\":2,\"policy_schema_hash\":\"fa91524af6c806f43e1203db44bb068d5c3dc1e22c1829703d54a03ed5cb8788\",\"indexed_documents\":0,\"certified_source_bytes\":0,\"sources\":[],\"core_record_aggregates\":[],\"source_routes\":[{\"route_identity\":\"abababababababababababababababababababababababababababababababab\",\"sources\":[],\"missing\":null}]}",
    );
    assert_eq!(
        manifest.generation_id().unwrap(),
        "348e8629b8db4b15f3445b916a7eb8e87ddaeb3a3cd1be3696d9d99b24ce6bed"
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
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitPayload {
pub version: u32,
pub generation_id: String,
pub publication_metadata: Option<String>,
