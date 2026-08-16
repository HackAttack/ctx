use super::*;

#[test]
fn destructive_precommit_truncate_and_replacement_preserve_last_good_generation_atomically() {
    // Same-object rewrites are excluded by the Codex append-only provider
    // contract and are covered by the explicit trust-boundary test in shared
    // JSONL. Observable truncation and object replacement must still fail the
    // terminal fence atomically.
    for mutation in ["truncate", "replacement"] {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index_root = temp.path().join("index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fb000-0000-7000-8000-000000000041";
        let path = session_path(&sessions, native_session_id);
        write_session(
            &sessions,
            native_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
            [message("lastgooduniquetoken")],
        );
        let registry = register_tree(&[&sessions]);
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        let before = VerifiedIndex::open(&index_root).unwrap();
        let generation = before.generation_id().to_owned();
        let snapshot = source_snapshot(&before, native_session_id, "lastgooduniquetoken");
        drop(before);

        let mutate = path.clone();
        let replacement = path.with_extension("replacement");
        if mutation == "replacement" {
            fs::write(
                &replacement,
                jsonl_bytes([
                    session_meta(
                        native_session_id,
                        ProviderNativeSessionRelationship::Root,
                        None,
                    ),
                    message("replacementuniquetoken"),
                ]),
            )
            .unwrap();
        }
        set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
            destructively_mutate_session(&mutate, &replacement, mutation);
        });
        match refresh_source_backed_generation(&index_root, &registry, writer_options()) {
            Ok(failed) => {
                assert_eq!(failed.failed_routes.len(), 1, "{mutation}");
                assert!(failed.failed_routes[0].carried_forward, "{mutation}");
            }
            Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
                assert_eq!(
                    source.kind,
                    SourceBackedRouteErrorKind::InvalidSource,
                    "{mutation}"
                );
            }
            Err(error) => panic!("unexpected {mutation} failure: {error:?}"),
        }
        let retained = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(retained.generation_id(), generation, "{mutation}");
        assert_eq!(
            source_snapshot(&retained, native_session_id, "lastgooduniquetoken"),
            snapshot,
            "{mutation}"
        );
        assert!(retained
            .search_event_candidates("replacementuniquetoken", 8)
            .unwrap()
            .is_empty());
    }
}
