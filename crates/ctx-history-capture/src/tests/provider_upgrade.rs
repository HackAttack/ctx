use super::support::*;

#[test]
fn provider_import_reuses_pre_identity_session_with_exact_source_path_proof() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "pre-identity-provider-session";
    let source_format = "claude_projects_jsonl_tree";
    let raw_source_path = temp
        .path()
        .join("projects/workspace/pre-identity-provider-session.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_source_id = provider_source_uuid(provider, provider_session_id);
    let scoped_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        source_format,
        Some(&raw_source_path),
    );
    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    assert_ne!(legacy_source_id, scoped_source_id);

    store
        .upsert_capture_source(&CaptureSource {
            id: legacy_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/workspace/example".to_owned()),
                raw_source_path: Some(raw_source_path.clone()),
                source_format: None,
                source_root: Some(raw_source_path.clone()),
                source_identity: None,
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: occurred_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: legacy_session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(legacy_source_id),
            provider,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: None,
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();

    let capture = provider_collision_capture(
        provider,
        provider_session_id,
        source_format,
        &raw_source_path,
        occurred_at,
    );
    for iteration in 0..2 {
        let summary = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, capture.clone())],
                files_touched: vec![],
            },
            NormalizedProviderImportOptions::default(),
        )
        .unwrap_or_else(|err| panic!("import iteration {iteration} failed: {err:?}"));
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        assert_eq!(
            store
                .get_session(legacy_session_id)
                .unwrap()
                .capture_source_id,
            Some(scoped_source_id)
        );
        assert_eq!(
            store.events_for_session(legacy_session_id).unwrap().len(),
            1
        );
    }

    let different_path = temp
        .path()
        .join("projects/workspace/copied-pre-identity-provider-session.jsonl")
        .display()
        .to_string();
    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    source_format,
                    &different_path,
                    occurred_at + chrono::Duration::seconds(1),
                ),
            )],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}
