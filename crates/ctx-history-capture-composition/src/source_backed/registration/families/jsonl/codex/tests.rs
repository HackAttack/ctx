use super::*;
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
};

#[test]
fn codex_session_tree_registration_does_not_inventory_the_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let sessions = temp.path().join("sessions-not-created");
    let source = ProviderSource {
        provider: CaptureProvider::Codex,
        path: sessions,
        exists: true,
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();

    register_codex_session_tree_routes(
        &mut registry,
        vec![source],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    assert_eq!(registry.routes().count(), 1);
}
