use super::*;

#[test]
fn retired_semantic_v2_checkpoint_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("retiredv2", retired_semantic_v2_checkpoint);
}

#[test]
fn malformed_semantic_checkpoint_key_is_inert_and_append_matches_cold() {
    assert_legacy_provider_checkpoint_is_inert("malformedkey", |_| TypedKey::U64(2));
}

#[test]
fn overlapping_automatic_and_explicit_routes_keep_selected_generation_ownership() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000013";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("automatic-explicit-cold-marker")],
    );

    let mut registry = register_tree(&[&sessions]);
    add_explicit_route(&mut registry, &path);
    let automatic_route = route_identity(&registry, &sessions);
    let explicit_route = route_identity(&registry, &path);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("automatic-explicit-cold-marker", 8)
            .unwrap()
            .len(),
        1
    );

    append_event(&path, message("explicit-only-append-marker"));
    let explicit = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [explicit_route],
    )
    .unwrap();
    assert!(explicit.failed_routes.is_empty());
    assert_eq!(explicit.sources.len(), 1);

    append_event(&path, message("automatic-only-append-marker"));
    let automatic = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [automatic_route],
    )
    .unwrap();
    assert!(automatic.failed_routes.is_empty());
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    for marker in [
        "automatic-explicit-cold-marker",
        "explicit-only-append-marker",
        "automatic-only-append-marker",
    ] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record.content.normalized_body.as_deref() == Some(marker))
                .count(),
            1,
            "missing or duplicated {marker}"
        );
    }
}
