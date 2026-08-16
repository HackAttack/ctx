use super::*;
use std::fs;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

fn core_publication_fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let route_identity = "ab".repeat(32);
    let publication = ctx_history_index::GenerationWriter::open(
        data_root.join("search/lexical"),
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit_with_publication_metadata(
        |_| true,
        |context| {
            let generation_id = context.generation_id().to_owned();
            let route = ctx_history_index::SourceRouteIdentity::from_sha256(
                route_identity.clone(),
            )
            .map_err(|error| {
                ctx_history_index::IndexError::PublicationMetadata(error.to_string())
            })?;
            let receipt = ctx_history_refresh::SourceBackedRefreshReceipt {
                previous_generation: None,
                published_generation: generation_id.clone(),
                generation_changed: true,
                published_explicit_source_catalog: None,
                current: ctx_history_refresh::SourceBackedRefreshCurrent::default(),
                route_results: vec![ctx_history_refresh::SourceBackedRefreshRouteResult::succeeded(
                    route_identity.clone(),
                    true,
                )],
                zero_source_authority: vec![
                    ctx_history_refresh::SourceBackedZeroSourceAuthority {
                        generation_id,
                        route_identity: route,
                        kind: ctx_history_refresh::SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                    },
                ],
                catalog_route_bindings: Vec::new(),
            };
            serde_json::to_vec(&json!({
                "version": ctx_history_refresh::SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                "request_id": "core-publication",
                "operation": "refresh",
                "refresh_scope": {"kind": "all"},
                "receipt": receipt.to_json(),
                "route_observations": [null],
                "route_controls": {},
            }))
            .map_err(|error| ctx_history_index::IndexError::PublicationMetadata(error.to_string()))
        },
    )
    .unwrap();
    let generation_id = publication.receipt().generation_id.clone();
    let catalog = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "core_refresh",
            "status": "completed",
            "request_id": "core-publication",
            "request_state": "published",
            "previous_generation": null,
            "published_generation": generation_id,
            "requested_explicit_source_catalog": catalog.to_json(),
            "published_explicit_source_catalog": catalog.to_json(),
            "generation_changed": true,
            "certified_source_count": 0,
            "certified_source_bytes": 0,
            "receipt": {
                "previous_generation": null,
                "published_generation": generation_id,
                "generation_changed": true,
                "published_explicit_source_catalog": catalog.to_json(),
                "current": {
                    "current_source_count": 0,
                    "current_indexed_documents": 0,
                    "current_complete_records": 0,
                    "current_retained_records": 0,
                    "current_rejected_records": 0,
                    "current_ignored_records": 0,
                    "current_certified_source_bytes": 0,
                    "current_sources_with_rejections": 0,
                    "removed_source_count": 0,
                },
            },
            "progress": {
                "phase": "committed",
                "completed_sources": 0,
                "total_sources": 0,
            },
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();
    (temp, data_root, generation_id)
}

fn publish_changed_core_generation(data_root: &Path) -> String {
    let source = ctx_history_core::SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("status-snapshot-race.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let native_session = TypedKey::utf8("status-snapshot-race-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id(
        "message",
        TypedKey::utf8("status-snapshot-race-event").unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        "status-snapshot-race-v1",
        "new generation published during status assembly",
    )
    .unwrap();
    record.provider_session_id = Some("status-snapshot-race-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.role = Some("assistant".to_owned());
    record.validate_contract().unwrap();

    let mut writer = GenerationWriter::open(
        data_root.join("search/lexical"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    let observation = SourceObservation::new(source, "status-snapshot-race-v1", vec![2]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "status-snapshot-race-v1",
                [2; 32],
                ScannedSourceCounts {
                    complete_records: 1,
                    retained_records: 1,
                    indexed_documents: 1,
                    certified_bytes: 128,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

#[test]
fn status_contract_has_no_resolver_or_source_manifest_authority() {
    let production = include_str!("source_status.rs");
    assert!(!production.contains("resolver_report"));
    assert!(!production.contains("\"resolver\""));
    assert!(!production.contains("source_manifest"));
}

#[test]
fn pristine_source_status_is_read_only_and_exposes_stable_paths() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("missing");

    let status =
        source_epoch_status_report(&data_root, &AppConfig::default()).expect("source status");

    assert!(!data_root.exists());
    assert_eq!(
        status.report["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert_eq!(
        status.report["semantic"]["flat_f32"]["path"],
        json!(data_root.join("search/semantic"))
    );
    assert!(status.report.get("prior_epoch").is_none());
}

#[test]
fn refresh_report_preserves_optional_active_source_record_and_byte_progress() {
    let job = json!({
        "request_state": "running",
        "request_id": "logical-request",
        "logical_request_id": "logical-request",
        "logical_phase": "attached",
        "physical_attempt_id": "physical-attempt",
        "physical_attempt_state": "running",
        "progress_owner_request_id": "progress-owner",
        "progress_owner_attempt_state": "running",
        "structured_outcome": {"code": "exact-engine-value"},
        "progress": {
            "phase": "refreshing",
            "completed_sources": 2,
            "total_sources": 6,
            "current_source": "source.db",
            "completed_records": 1234,
            "completed_bytes": 4 * 1024 * 1024,
        },
    });
    let daemon = json!({"running": true});

    let report = refresh_report(Some(&job), None, &daemon);

    assert_eq!(report["progress"]["current_source"], "source.db");
    assert_eq!(report["progress"]["completed_records"], 1234);
    assert_eq!(report["progress"]["completed_bytes"], 4 * 1024 * 1024);
    for field in [
        "logical_request_id",
        "logical_phase",
        "physical_attempt_id",
        "physical_attempt_state",
        "progress_owner_request_id",
        "progress_owner_attempt_state",
        "structured_outcome",
    ] {
        assert_eq!(report[field], job[field], "field={field}");
    }
}

#[test]
fn source_daemon_report_preserves_semantic_terminal_job_facts() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join(crate::config::CONFIG_FILE),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();
    super::super::paths_status::write_daemon_job_status(
        &daemon_semantic_job_path(&data_root),
        &json!({
            "status": "skipped",
            "reason": "model_cache_missing",
            "last_run_at_ms": 1,
        }),
    )
    .unwrap();

    let config = crate::config::AppConfig::load(&data_root).unwrap();
    let daemon = source_daemon_report(&data_root, &config);
    let jobs = daemon["jobs"].as_object().unwrap();
    assert!(jobs.contains_key("core_refresh"), "{daemon:#}");
    assert!(jobs.contains_key("semantic_index"), "{daemon:#}");
    assert!(!jobs.contains_key("history_refresh"), "{daemon:#}");
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_status"],
        "skipped"
    );
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_reason"],
        "model_cache_missing"
    );
    if super::super::semantic_query_service_supported() {
        assert_eq!(daemon["jobs"]["semantic_index"]["status"], "skipped");
        assert_eq!(
            daemon["jobs"]["semantic_index"]["reason"],
            "model_cache_missing"
        );
    }
}

#[test]
fn lexical_state_depends_only_on_verified_generation_policy_identity() {
    assert_eq!(lexical_state(true), ("ready", None));
    assert_eq!(
        lexical_state(false),
        ("stale", Some("generation_policy_mismatch"))
    );
}

#[test]
fn refresh_report_uses_typed_pending_ready_stale_and_unavailable_states() {
    let daemon = json!({"running": true});
    for request_state in ["admission_pending", "queued", "running"] {
        let pending = refresh_report(
            Some(&json!({"request_state": request_state})),
            Some("generation-1"),
            &daemon,
        );
        assert_eq!(pending["status"], "pending", "{request_state}");
    }
    let ready = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
        })),
        Some("generation-1"),
        &daemon,
    );
    let stale = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-0",
            "certified_source_count": 2,
            "certified_source_bytes": 4096,
            "timings_us": {"discovery": 11, "scan_stage": 22, "commit": 33},
        })),
        Some("generation-1"),
        &daemon,
    );
    let unavailable = refresh_report(None, None, &json!({"running": false}));

    assert_eq!(ready["status"], "ready");
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["certified_source_count"], 2);
    assert_eq!(stale["certified_source_bytes"], 4096);
    assert_eq!(stale["timings_us"]["commit"], 33);
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(unavailable["reason"], "daemon_unavailable");
}

#[test]
fn refresh_report_keeps_published_sources_distinct_from_route_inventory() {
    let report = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "source_count": 1,
            "scanned_routes": 38,
            "unsupported_routes": 37,
            "progress": {
                "phase": "published",
                "completed_sources": 38,
                "total_sources": 38,
                "total_sources_known": true,
            },
            "receipt": {
                "outcome": "completed",
                "current": {"current_source_count": 2},
            },
        })),
        Some("generation-1"),
        &json!({"running": true}),
    );

    assert_eq!(report["source_count"], 1);
    assert_eq!(report["current"]["current_source_count"], 2);
    assert_eq!(report["scanned_routes"], 38);
    assert_eq!(report["unsupported_routes"], 37);
    assert_eq!(report["progress"]["total_sources"], 38);
}

#[test]
fn admission_pending_is_active_with_existing_and_empty_generations() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "status": "running",
            "request_id": "admission-existing",
            "request_state": "admission_pending",
            "published_generation": generation_id,
        }),
    )
    .unwrap();

    let existing = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(existing.report["refresh"]["status"], "pending");
    assert_eq!(existing.report["lexical"]["status"], "ready");
    assert_eq!(
        existing.report["lexical"]["request_state"],
        "admission_pending"
    );

    let empty = tempfile::tempdir().unwrap();
    let empty_root = empty.path().join("data");
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&empty_root),
        &json!({
            "status": "running",
            "request_id": "admission-empty",
            "request_state": "admission_pending",
        }),
    )
    .unwrap();
    let empty = source_epoch_status_report(&empty_root, &AppConfig::default()).unwrap();
    assert_eq!(empty.report["refresh"]["status"], "pending");
    assert_eq!(empty.report["lexical"]["status"], "pending");
    assert_eq!(
        empty.report["lexical"]["reason"],
        "generation_not_published"
    );
}

#[test]
fn authoritative_empty_stays_query_ready_when_the_latest_refresh_failed() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(&data_root),
        &json!({
            "status": "failed",
            "request_id": "failed-after-authoritative-empty",
            "request_state": "failed",
            "published_generation": generation_id,
            "last_error": "all_provider_terminal_coverage_unavailable",
        }),
    )
    .unwrap();

    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "ready");
    assert_eq!(status.report["history_epoch"]["status"], "ready");
    assert_eq!(status.report["refresh"]["status"], "unavailable");
    assert_eq!(status.report["refresh"]["reason"], "core_refresh_failed");
    assert_eq!(status.indexed_items, Some(0));
}

#[test]
fn legacy_zero_source_publication_is_not_projected_as_ready() {
    let (_temp, data_root, generation_id) = core_publication_fixture();
    let index_root = data_root.join("search/lexical");
    let current = VerifiedIndex::open(&index_root).unwrap();
    let mut metadata: Value =
        serde_json::from_slice(current.publication_metadata().unwrap()).unwrap();
    metadata["version"] = json!(1);
    metadata["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("zero_source_authority");
    metadata.as_object_mut().unwrap().remove("route_controls");
    drop(current);
    let writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();

    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "unavailable");
    assert_eq!(
        status.report["lexical"]["reason"],
        "zero_source_publication_uncertified"
    );
    assert_eq!(status.report["history_epoch"]["status"], "unavailable");
    assert_eq!(status.indexed_items, None);
    assert_eq!(status.indexed_sources, None);
}

#[test]
fn published_record_rejections_are_ready_but_remain_diagnostic() {
    let daemon = json!({"running": true});
    let report = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "receipt": {
                "outcome": "completed_with_rejections",
                "source_failure_total": 0,
                "rejected_record_total": 1,
                "current": {
                    "current_rejected_records": 1,
                },
            },
            "structured_outcome": {"retryable": false},
        })),
        Some("generation-1"),
        &daemon,
    );

    assert_eq!(report["status"], "ready", "{report:#}");
    assert_eq!(report["outcome"], "completed_with_rejections");
    assert_eq!(report["current"]["current_rejected_records"], 1);
    assert_eq!(
        current_rejected_record_count(&json!({"refresh": report})),
        1
    );
}

#[test]
fn source_failures_and_combined_diagnostics_remain_partial() {
    let daemon = json!({"running": true});
    for (outcome, rejected_records) in [
        ("completed_with_source_failures", 0),
        ("completed_with_rejections_and_source_failures", 1),
    ] {
        let report = refresh_report(
            Some(&json!({
                "request_state": "published",
                "published_generation": "generation-1",
                "receipt": {
                    "outcome": outcome,
                    "source_failure_total": 1,
                    "rejected_record_total": rejected_records,
                    "current": {
                        "current_rejected_records": rejected_records,
                    },
                },
            })),
            Some("generation-1"),
            &daemon,
        );
        assert_eq!(report["status"], "partial", "{outcome}: {report:#}");
        assert_eq!(report["outcome"], outcome);
    }
}

#[test]
fn retryable_published_failure_remains_partial() {
    let daemon = json!({"running": true});
    let report = refresh_report(
        Some(&json!({
            "request_state": "published",
            "published_generation": "generation-1",
            "receipt": {
                "outcome": "completed_with_rejections",
                "source_failure_total": 0,
                "current": {"current_rejected_records": 1},
            },
            "structured_outcome": {"retryable": true},
        })),
        Some("generation-1"),
        &daemon,
    );

    assert_eq!(report["status"], "partial", "{report:#}");
}

#[test]
fn catalog_status_reports_automatic_roots_and_request_scoped_explicit_overlays() {
    let temp = tempfile::tempdir().unwrap();
    let index_root = temp.path().join("search/lexical");
    let generation_id = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
    let index = VerifiedIndex::open(&index_root).unwrap();
    let ready = catalog_report(Some(&generation_id), Some(&index));
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["authority"], "automatic_provider_registry");
    assert_eq!(ready["explicit_import_authority"], "request_scoped_overlay");
    assert_eq!(ready["persisted_explicit_roots"], false);

    let pending = catalog_report(None, None);
    assert_eq!(pending["status"], "pending");
}
