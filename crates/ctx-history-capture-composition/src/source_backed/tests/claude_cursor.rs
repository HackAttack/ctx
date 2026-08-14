use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, CoreDiscoveryExclusion, CoreRecord, StableEntityId};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::{
    provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook,
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRouteSelection, SourceBackedSourceFailureClass,
};

fn assert_no_linked_repository_evidence(record: &CoreRecord) {
    assert!(record.repository_bindings.is_empty());
    assert!(record.repository_file_invocation_evidence.is_empty());
    assert!(record.repository_file_observations.is_empty());
    assert!(record.repository_vcs_observations.is_empty());
}

fn transcript_path(root: &Path, project: &str, session: &str) -> PathBuf {
    root.join("projects")
        .join(project)
        .join("agent-transcripts")
        .join(session)
        .join(format!("{session}.jsonl"))
}

fn repository(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("repo");
    fs::create_dir(&path).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    fs::create_dir(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), "pub fn native() {}\n").unwrap();
    path
}

fn write_transcript(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut encoded = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut encoded, row).unwrap();
        encoded.push(b'\n');
    }
    fs::write(path, encoded).unwrap();
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn message(role: &str, timestamp: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "role": role,
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Cursor,
            path: root.to_path_buf(),
            exists: true,
            source_format: "cursor_agent_transcript_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn indexed_event_ids(index: &Path, native_session_id: &str) -> Vec<StableEntityId> {
    indexed_records(index, native_session_id)
        .into_iter()
        .map(|record| record.event_id)
        .collect()
}

fn indexed_records(index: &Path, native_session_id: &str) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = Vec::new();
    for source in &verified.manifest().sources {
        if source.observation().source().provider() != "cursor" {
            continue;
        }
        let page = verified
            .core_source_event_page(source.observation().source(), None, 256)
            .unwrap();
        for item in page.items {
            let record = verified
                .core_record_by_id(item.event_id.as_uuid())
                .unwrap()
                .unwrap();
            if record.provider_session_id.as_deref() == Some(native_session_id) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn assert_ids_preserved(previous: &[StableEntityId], current: &[StableEntityId]) {
    for event_id in previous {
        assert!(current.contains(event_id), "prior event identity changed");
    }
}

fn assert_all_ids_distinct(event_ids: &[StableEntityId]) {
    for (index, event_id) in event_ids.iter().enumerate() {
        assert!(!event_ids[index + 1..].contains(event_id));
    }
}

#[test]
fn cursor_append_projects_only_suffix_and_probes_pinned_base_for_duplicate_occurrences() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cursor-data");
    let transcript = transcript_path(&root, "project", "native-session");
    let first = message("user", "2026-07-31T12:00:00Z", "first");
    let second = message("assistant", "2026-07-31T12:00:01Z", "second");
    write_transcript(&transcript, &[first]);
    let registry = registry(&root);
    let index = temp.path().join("index");
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        "native-session",
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_signature_records();
    ctx_history_provider_claude_cursor::test_support::cursor::reset_base_identity_probes();
    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            "native-session"
        ),
        1
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::base_identity_probes(),
        0
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::signature_records(),
        0,
        "a singleton native session must not be pre-parsed for route comparison"
    );
    let cold_ids = indexed_event_ids(&index, "native-session");

    append_transcript(&transcript, &second);
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        "native-session",
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_signature_records();
    ctx_history_provider_claude_cursor::test_support::cursor::reset_base_identity_probes();
    let appended = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 2);
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            "native-session"
        ),
        1,
        "Cursor append work must remain bounded to the validated suffix"
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::signature_records(),
        0,
        "singleton append discovery must not rescan transcript content"
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::base_identity_probes(),
        1
    );
    let appended_ids = indexed_event_ids(&index, "native-session");
    assert_ids_preserved(&cold_ids, &appended_ids);

    append_transcript(&transcript, &second);
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        "native-session",
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_signature_records();
    ctx_history_provider_claude_cursor::test_support::cursor::reset_base_identity_probes();
    let first_duplicate =
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(first_duplicate.commit.indexed_documents, 3);
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            "native-session"
        ),
        1
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::signature_records(),
        0
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::base_identity_probes(),
        2
    );
    let first_duplicate_ids = indexed_event_ids(&index, "native-session");
    assert_ids_preserved(&appended_ids, &first_duplicate_ids);
    assert_all_ids_distinct(&first_duplicate_ids);

    append_transcript(&transcript, &second);
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        "native-session",
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_signature_records();
    ctx_history_provider_claude_cursor::test_support::cursor::reset_base_identity_probes();
    let second_duplicate =
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(second_duplicate.commit.indexed_documents, 4);
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            "native-session"
        ),
        1
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::signature_records(),
        0
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::base_identity_probes(),
        3
    );
    let second_duplicate_ids = indexed_event_ids(&index, "native-session");
    assert_ids_preserved(&first_duplicate_ids, &second_duplicate_ids);
    assert_all_ids_distinct(&second_duplicate_ids);
}

#[test]
fn cursor_late_duplicate_result_forces_replacement_and_corrects_the_earlier_result() {
    let temp = TempDir::new().unwrap();
    let repo = repository(&temp);
    let root = temp.path().join("cursor-data");
    let native_session_id = "late-duplicate-session";
    let transcript = transcript_path(&root, "project", native_session_id);
    let call_id = "late-duplicate-result";
    let call = json!({
        "timestamp": "2026-07-31T12:00:00Z",
        "role": "assistant",
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": call_id,
            "name": "run_shell_command",
            "input": {
                "command": "ctx search late-duplicate",
                "workdir": repo,
                "path": "src/lib.rs",
            }
        }]}
    });
    let result = |content: &str, timestamp: &str| {
        json!({
            "timestamp": timestamp,
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": false
            }]}
        })
    };
    write_transcript(
        &transcript,
        &[
            call,
            result(
                "first late duplicate Cursor payload",
                "2026-07-31T12:00:01Z",
            ),
        ],
    );
    let registry = registry(&root);
    let index = temp.path().join("index");

    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let initial = indexed_records(&index, native_session_id);
    assert_eq!(initial.len(), 2);
    assert!(initial.iter().all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    }));
    assert_eq!(initial[1].repository_bindings.len(), 1);
    assert_eq!(initial[1].repository_file_observations.len(), 1);
    let initial_ids = initial
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();

    append_transcript(
        &transcript,
        &result(
            "second late duplicate Cursor payload",
            "2026-07-31T12:00:02Z",
        ),
    );
    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let corrected = indexed_records(&index, native_session_id);
    assert_eq!(corrected.len(), 3);
    assert_eq!(corrected[0].event_id, initial_ids[0]);
    assert_eq!(corrected[1].event_id, initial_ids[1]);
    assert_eq!(
        corrected[0].content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(corrected[1].content.discovery_exclusion, None);
    assert_eq!(corrected[2].content.discovery_exclusion, None);
    assert!(corrected[1]
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .contains("first late duplicate Cursor payload"));
    assert!(corrected[2]
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .contains("second late duplicate Cursor payload"));
    assert_no_linked_repository_evidence(&corrected[1]);
    assert_no_linked_repository_evidence(&corrected[2]);
}

#[test]
fn cursor_appended_malformed_linkage_matrix_retracts_prior_result_authority() {
    let oversized = "x".repeat(512 + 1);
    let cases = [
        (
            "literal-duplicate",
            r#""tool_use_id":"same-object-ambiguous-result","tool_use_id":"other""#.to_owned(),
        ),
        (
            "escaped-duplicate",
            r#""tool_use_id":"same-object-ambiguous-result","tool_\u0075se_id":"other""#.to_owned(),
        ),
        ("non-string", r#""tool_use_id":7"#.to_owned()),
        ("empty", r#""tool_use_id":"""#.to_owned()),
        ("oversized", format!(r#""tool_use_id":"{oversized}""#)),
    ];
    for (name, selector) in cases {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let root = temp.path().join("cursor-data");
        let native_session_id = format!("same-object-ambiguous-{name}-session");
        let transcript = transcript_path(&root, "project", &native_session_id);
        let call_id = "same-object-ambiguous-result";
        let call = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": call_id,
                "name": "run_shell_command",
                "input": {
                    "command": "ctx search same-object-ambiguous",
                    "workdir": repo,
                    "path": "src/lib.rs",
                }
            }]}
        });
        let first = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": "first authoritative Cursor payload",
                "is_error": false
            }]}
        });
        write_transcript(&transcript, &[call, first]);
        let registry = registry(&root);
        let index = temp.path().join("index");

        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        let initial = indexed_records(&index, &native_session_id);
        assert_eq!(initial.len(), 2);
        assert!(initial.iter().all(|record| {
            record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        }));
        assert_eq!(initial[1].repository_bindings.len(), 1, "{name}");
        assert_eq!(initial[1].repository_file_observations.len(), 1, "{name}");
        let initial_invocation_id = initial
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("ctx search same-object-ambiguous"))
            })
            .unwrap()
            .event_id;
        let initial_result_id = initial
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("first authoritative Cursor payload"))
            })
            .unwrap()
            .event_id;

        let ambiguous = format!(
            r#"{{"timestamp":"2026-07-31T12:00:02Z","role":"user","message":{{"role":"user","content":[{{"type":"tool_result",{selector},"content":"ambiguous Cursor payload","is_error":false}}]}}}}"#
        );
        let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
        file.write_all(ambiguous.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();

        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        let corrected = indexed_records(&index, &native_session_id);
        assert_eq!(corrected.len(), 3);
        let corrected_invocation = corrected
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("ctx search same-object-ambiguous"))
            })
            .unwrap();
        let corrected_result = corrected
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("first authoritative Cursor payload"))
            })
            .unwrap();
        let ambiguous_result = corrected
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("ambiguous Cursor payload"))
            })
            .unwrap();
        assert_eq!(
            corrected_invocation.event_id, initial_invocation_id,
            "{name}"
        );
        assert_eq!(corrected_result.event_id, initial_result_id, "{name}");
        assert_eq!(
            corrected_invocation.content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
        assert_eq!(corrected_result.content.discovery_exclusion, None, "{name}");
        assert_eq!(ambiguous_result.content.discovery_exclusion, None, "{name}");
        assert_no_linked_repository_evidence(corrected_result);
        assert_no_linked_repository_evidence(ambiguous_result);
    }
}

#[test]
fn cursor_projector_preflight_rejects_same_length_interpass_rewrite() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cursor-data");
    let native_session_id = "preflight-race";
    let transcript = transcript_path(&root, "project", native_session_id);
    write_transcript(
        &transcript,
        &[message("user", "2026-07-31T12:00:00Z", "stable baseline")],
    );
    let registry = registry(&root);
    let index = temp.path().join("index");
    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());

    append_transcript(
        &transcript,
        &message("assistant", "2026-07-31T12:00:01Z", "race-before"),
    );
    let hook_path = fs::canonicalize(&transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(failed.failed_routes[0].carried_forward);
    let retained = indexed_records(&index, native_session_id);
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].content.normalized_body.as_deref(),
        Some("stable baseline")
    );
}

#[test]
fn cursor_ambiguous_leaf_replacement_does_not_expand_clean_sibling_work() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("cursor-data");
    let ambiguous_session = "ambiguous-leaf";
    let clean_session = "clean-leaf";
    let ambiguous_path = transcript_path(&root, "project-a", ambiguous_session);
    let clean_path = transcript_path(&root, "project-b", clean_session);
    let call_id = "duplicate-result";
    let call = json!({
        "timestamp": "2026-07-31T12:00:00Z",
        "role": "assistant",
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": call_id,
            "name": "run_shell_command",
            "input": {"command": "ctx search leaf-local"}
        }]}
    });
    let result = |content: &str, timestamp: &str| {
        json!({
            "timestamp": timestamp,
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": false
            }]}
        })
    };
    write_transcript(
        &ambiguous_path,
        &[call, result("first result", "2026-07-31T12:00:01Z")],
    );
    write_transcript(
        &clean_path,
        &[message("user", "2026-07-31T12:00:00Z", "clean base")],
    );
    let registry = registry(&root);
    let index = temp.path().join("index");
    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();

    append_transcript(
        &ambiguous_path,
        &result("second result", "2026-07-31T12:00:02Z"),
    );
    append_transcript(
        &clean_path,
        &message("assistant", "2026-07-31T12:00:01Z", "clean append"),
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        ambiguous_session,
    );
    ctx_history_provider_claude_cursor::test_support::cursor::reset_projected_records(
        clean_session,
    );

    let appended = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            ambiguous_session
        ),
        3
    );
    assert_eq!(
        ctx_history_provider_claude_cursor::test_support::cursor::take_projected_records(
            clean_session
        ),
        1,
        "an ambiguous sibling must not force a clean append leaf into replacement"
    );
}

fn initialized_test_repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for arguments in [
        &["init", "-q"][..],
        &["config", "user.name", "ctx test"],
        &["config", "user.email", "ctx@example.invalid"],
    ] {
        assert!(std::process::Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(temp.path().join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [&["add", "tracked.txt"][..], &["commit", "-qm", "fixture"]] {
        assert!(std::process::Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
    }
    temp
}

fn claude_test_registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Claude,
            path: root.to_path_buf(),
            exists: true,
            source_format: "claude_projects_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn claude_test_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn write_claude_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_claude_transcript(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn indexed_claude_records(index: &Path, native_session_id: &str) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = Vec::new();
    for source in &verified.manifest().sources {
        if source.observation().source().provider() != "claude" {
            continue;
        }
        let page = verified
            .core_source_event_page(source.observation().source(), None, 256)
            .unwrap();
        for item in page.items {
            let record = verified
                .core_record_by_id(item.event_id.as_uuid())
                .unwrap()
                .unwrap();
            if record.provider_session_id.as_deref() == Some(native_session_id) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| record.event_sequence);
    records
}

#[test]
fn claude_late_duplicate_result_forces_replacement_and_retracts_prior_exclusion() {
    let temp = tempfile::tempdir().unwrap();
    let repository = initialized_test_repository();
    let projects = temp.path().join("projects");
    let transcript = projects
        .join("project")
        .join("late-duplicate-session.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "late-duplicate-session";
    let call_id = "late-duplicate-result";
    let call = serde_json::json!({
        "type": "assistant",
        "uuid": "late-duplicate-call",
        "sessionId": native_session_id,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": call_id,
            "name": "Bash",
            "input": {
                "command": "ctx search late-duplicate",
                "workdir": repository.path(),
            }
        }]},
    });
    let result = |uuid: &str, content: &str| {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "sessionId": native_session_id,
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": false
            }]},
        })
    };
    write_claude_transcript(
        &transcript,
        &[
            call,
            result(
                "late-duplicate-first",
                "first late duplicate Claude payload",
            ),
        ],
    );
    let registry = claude_test_registry(&projects);

    refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    let initial = indexed_claude_records(&index, native_session_id);
    assert_eq!(initial.len(), 2);
    assert!(initial.iter().all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    }));
    assert_eq!(initial[1].repository_bindings.len(), 1);
    let initial_ids = initial
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();

    append_claude_transcript(
        &transcript,
        &result(
            "late-duplicate-second",
            "second late duplicate Claude payload",
        ),
    );
    refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    let corrected = indexed_claude_records(&index, native_session_id);
    assert_eq!(corrected.len(), 3);
    assert_eq!(corrected[0].event_id, initial_ids[0]);
    assert_eq!(corrected[1].event_id, initial_ids[1]);
    assert_eq!(
        corrected[0].content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(corrected[1].content.discovery_exclusion, None);
    assert_eq!(corrected[2].content.discovery_exclusion, None);
    assert_eq!(
        corrected[1].content.normalized_body.as_deref(),
        Some("first late duplicate Claude payload")
    );
    assert_eq!(
        corrected[2].content.normalized_body.as_deref(),
        Some("second late duplicate Claude payload")
    );
    assert_no_linked_repository_evidence(&corrected[1]);
    assert_no_linked_repository_evidence(&corrected[2]);
}

#[test]
fn claude_projector_preflight_rejects_same_length_interpass_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let transcript = projects.join("project").join("preflight-race.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "preflight-race";
    write_claude_transcript(
        &transcript,
        &[serde_json::json!({
            "type": "user",
            "uuid": "stable-record",
            "sessionId": native_session_id,
            "message": {"role": "user", "content": "stable baseline"},
        })],
    );
    let registry = claude_test_registry(&projects);
    let initial =
        refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());

    append_claude_transcript(
        &transcript,
        &serde_json::json!({
            "type": "assistant",
            "uuid": "racing-record",
            "sessionId": native_session_id,
            "message": {"role": "assistant", "content": "race-before"},
        }),
    );
    let hook_path = fs::canonicalize(&transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(hook_path, after).unwrap();
    });

    let failed =
        refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(failed.failed_routes[0].carried_forward);
    let retained = indexed_claude_records(&index, native_session_id);
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].content.normalized_body.as_deref(),
        Some("stable baseline")
    );
}

#[test]
fn claude_trailing_malformed_terminal_makes_an_earlier_result_searchable() {
    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let transcript = projects
        .join("project")
        .join("trailing-terminal-session.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "trailing-terminal-session";
    let call_id = "trailing-terminal-result";
    let call = serde_json::json!({
        "type": "assistant",
        "uuid": "trailing-terminal-call",
        "sessionId": native_session_id,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": call_id,
            "name": "Bash",
            "input": {"command": "ctx search trailing-terminal"}
        }]},
    });
    let result = serde_json::json!({
        "type": "user",
        "uuid": "trailing-terminal-first",
        "sessionId": native_session_id,
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": "prior authoritative Claude payload",
            "is_error": false
        }]},
    });
    write_claude_transcript(&transcript, &[call, result.clone()]);
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    serde_json::to_writer(&mut file, &result).unwrap();
    file.write_all(b" trailing terminal bytes\n").unwrap();
    file.sync_all().unwrap();

    let registry = claude_test_registry(&projects);
    refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    let records = indexed_claude_records(&index, native_session_id);

    assert_eq!(records.len(), 2);
    let invocation = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ctx search trailing-terminal"))
        })
        .unwrap();
    let retained_result = records
        .iter()
        .find(|record| {
            record.content.normalized_body.as_deref() == Some("prior authoritative Claude payload")
        })
        .unwrap();
    assert_eq!(
        invocation.content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(retained_result.content.discovery_exclusion, None);
}

#[test]
fn claude_appended_duplicate_member_terminal_retracts_prior_result_exclusion() {
    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let transcript = projects
        .join("project")
        .join("ambiguous-terminal-session.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "ambiguous-terminal-session";
    let call_id = "ambiguous-terminal-result";
    let call = serde_json::json!({
        "type": "assistant",
        "uuid": "ambiguous-terminal-call",
        "sessionId": native_session_id,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": call_id,
            "name": "Bash",
            "input": {"command": "ctx search ambiguous-terminal"}
        }]},
    });
    let first_result = serde_json::json!({
        "type": "user",
        "uuid": "ambiguous-terminal-first",
        "sessionId": native_session_id,
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": "first authoritative Claude payload",
            "is_error": false
        }]},
    });
    write_claude_transcript(&transcript, &[call, first_result]);
    let registry = claude_test_registry(&projects);

    refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    let initial = indexed_claude_records(&index, native_session_id);
    assert_eq!(initial.len(), 2);
    assert!(initial.iter().all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    }));
    let initial_invocation = initial
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ctx search ambiguous-terminal"))
        })
        .unwrap();
    let initial_result = initial
        .iter()
        .find(|record| {
            record.content.normalized_body.as_deref() == Some("first authoritative Claude payload")
        })
        .unwrap();

    let ambiguous_result = format!(
        r#"{{"type":"user","uuid":"ambiguous-terminal-second","sessionId":"{native_session_id}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{call_id}","content":{{"result":"discarded duplicate member","result":"ambiguous duplicate-member Claude payload"}},"is_error":false}}]}}}}"#,
    );
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    file.write_all(ambiguous_result.as_bytes()).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();

    refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
    let corrected = indexed_claude_records(&index, native_session_id);
    assert!(corrected.len() >= 3);
    let corrected_invocation = corrected
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ctx search ambiguous-terminal"))
        })
        .unwrap();
    let corrected_result = corrected
        .iter()
        .find(|record| {
            record.content.normalized_body.as_deref() == Some("first authoritative Claude payload")
        })
        .unwrap();
    assert_eq!(corrected_invocation.event_id, initial_invocation.event_id);
    assert_eq!(corrected_result.event_id, initial_result.event_id);
    assert_eq!(
        corrected_invocation.content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(corrected_result.content.discovery_exclusion, None);
    let ambiguous = corrected
        .iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ambiguous duplicate-member Claude payload"))
        })
        .collect::<Vec<_>>();
    assert!(!ambiguous.is_empty());
    assert!(ambiguous
        .iter()
        .all(|record| record.content.discovery_exclusion.is_none()));
}

#[test]
fn claude_appended_malformed_selector_matrix_retracts_prior_result_authority() {
    let repository = initialized_test_repository();
    let call_id = "selector-alias-result";
    let oversized = "x".repeat(257);
    let cases = [
        (
            "conflicting",
            format!(r#""toolUseId":"{call_id}","toolCallId":"other""#),
            true,
        ),
        (
            "duplicate-alias",
            format!(r#""toolCallId":"{call_id}","toolCallId":"other""#),
            false,
        ),
        ("non-string", r#""toolCallId":7"#.to_owned(), true),
        ("empty", r#""toolCallId":"""#.to_owned(), true),
        ("oversized", format!(r#""toolCallId":"{oversized}""#), true),
    ];
    for (name, selector, appended_is_retained) in cases {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let native_session_id = format!("selector-alias-{name}-session");
        let transcript = projects
            .join("project")
            .join(format!("{native_session_id}.jsonl"));
        let index = temp.path().join("index");
        let call = serde_json::json!({
            "type": "assistant",
            "uuid": format!("selector-alias-{name}-call"),
            "sessionId": native_session_id,
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": call_id,
                "name": "Bash",
                "input": {
                    "command": "ctx search selector-alias",
                    "workdir": repository.path(),
                }
            }]},
        });
        let first_result = serde_json::json!({
            "type": "user",
            "uuid": format!("selector-alias-{name}-first"),
            "sessionId": native_session_id,
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": "first authoritative selector payload",
                "is_error": false
            }]},
        });
        write_claude_transcript(&transcript, &[call, first_result]);
        let registry = claude_test_registry(&projects);

        refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
        let initial = indexed_claude_records(&index, &native_session_id);
        assert_eq!(initial.len(), 2, "{name}");
        assert!(
            initial.iter().all(|record| {
                record.content.discovery_exclusion
                    == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
            }),
            "{name}"
        );
        assert_eq!(initial[1].repository_bindings.len(), 1, "{name}");
        let initial_invocation_id = initial
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("ctx search selector-alias"))
            })
            .unwrap()
            .event_id;
        let initial_result_id = initial
            .iter()
            .find(|record| {
                record.content.normalized_body.as_deref()
                    == Some("first authoritative selector payload")
            })
            .unwrap()
            .event_id;

        let ambiguous = format!(
            r#"{{"type":"user","uuid":"selector-alias-{name}-second","sessionId":"{native_session_id}","message":{{"role":"user","content":[{{"type":"tool_result",{selector},"content":"ambiguous selector alias payload","is_error":false}}]}}}}"#
        );
        let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
        file.write_all(ambiguous.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();

        refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();
        let corrected = indexed_claude_records(&index, &native_session_id);
        assert_eq!(
            corrected.len(),
            2 + usize::from(appended_is_retained),
            "{name}"
        );
        let corrected_invocation = corrected
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("ctx search selector-alias"))
            })
            .unwrap();
        let corrected_result = corrected
            .iter()
            .find(|record| {
                record.content.normalized_body.as_deref()
                    == Some("first authoritative selector payload")
            })
            .unwrap();
        let ambiguous_result = corrected.iter().find(|record| {
            record.content.normalized_body.as_deref() == Some("ambiguous selector alias payload")
        });
        assert_eq!(
            corrected_invocation.event_id, initial_invocation_id,
            "{name}"
        );
        assert_eq!(corrected_result.event_id, initial_result_id, "{name}");
        assert_eq!(
            corrected_invocation.content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
            "{name}"
        );
        assert_eq!(corrected_result.content.discovery_exclusion, None, "{name}");
        assert_no_linked_repository_evidence(corrected_result);
        assert_eq!(ambiguous_result.is_some(), appended_is_retained, "{name}");
        if let Some(ambiguous_result) = ambiguous_result {
            assert_eq!(ambiguous_result.content.discovery_exclusion, None, "{name}");
            assert_no_linked_repository_evidence(ambiguous_result);
        }
    }
}

#[test]
fn thinking_only_split_records_are_certified_as_ignored() {
    const PRIVATE_THINKING: &str = "private-thinking-must-not-enter-core";
    const PRIVATE_SIGNATURE: &str = "private-signature-must-not-enter-core";

    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "thinking-split-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    let assistant_message = |message_id: &str, content: serde_json::Value| {
        serde_json::json!({
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-fixture",
            "content": content,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {},
        })
    };
    write_claude_transcript(
        &transcript,
        &[
            serde_json::json!({
                "type": "assistant",
                "uuid": "thinking-record-before-text",
                "parentUuid": "prior-user-before-text",
                "sessionId": native_session_id,
                "requestId": "request-with-text",
                "timestamp": "2026-08-09T12:00:00Z",
                "cwd": "/fixture/project",
                "gitBranch": "main",
                "version": "fixture",
                "isSidechain": false,
                "userType": "external",
                "message": assistant_message("message-with-text", serde_json::json!([{
                    "type": "thinking",
                    "thinking": PRIVATE_THINKING,
                    "signature": PRIVATE_SIGNATURE,
                }])),
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "text-after-thinking",
                "parentUuid": "thinking-record-before-text",
                "sessionId": native_session_id,
                "requestId": "request-with-text",
                "timestamp": "2026-08-09T12:00:01Z",
                "cwd": "/fixture/project",
                "gitBranch": "main",
                "version": "fixture",
                "isSidechain": false,
                "userType": "external",
                "message": assistant_message("message-with-text", serde_json::json!([{
                    "type": "text",
                    "text": "visible split response",
                }])),
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "thinking-record-before-tool",
                "parentUuid": "prior-user-before-tool",
                "sessionId": native_session_id,
                "requestId": "request-with-tool",
                "timestamp": "2026-08-09T12:00:02Z",
                "cwd": "/fixture/project",
                "gitBranch": "main",
                "version": "fixture",
                "isSidechain": false,
                "userType": "external",
                "message": assistant_message("message-with-tool", serde_json::json!([{
                    "type": "thinking",
                    "thinking": PRIVATE_THINKING,
                    "signature": PRIVATE_SIGNATURE,
                }])),
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "tool-after-thinking",
                "parentUuid": "thinking-record-before-tool",
                "sessionId": native_session_id,
                "requestId": "request-with-tool",
                "timestamp": "2026-08-09T12:00:03Z",
                "cwd": "/fixture/project",
                "gitBranch": "main",
                "version": "fixture",
                "isSidechain": false,
                "userType": "external",
                "message": assistant_message("message-with-tool", serde_json::json!([{
                    "type": "tool_use",
                    "id": "split-tool-call",
                    "name": "Read",
                    "input": {"file_path": "src/lib.rs"},
                }])),
            }),
        ],
    );
    let registry = claude_test_registry(&projects);
    let index = temp.path().join("index");

    let receipt =
        refresh_source_backed_generation(&index, &registry, claude_test_writer_options()).unwrap();

    assert_eq!(receipt.sources.len(), 1);
    let counts = receipt.sources[0].counts();
    assert_eq!(counts.complete_records, 4);
    assert_eq!(counts.ignored_records, 2);
    assert_eq!(counts.rejected_records, 0);
    assert_eq!(counts.retained_records, 2);
    assert_eq!(counts.indexed_documents, 2);
    assert_eq!(receipt.commit.indexed_documents, 2);

    let records = indexed_claude_records(&index, native_session_id);
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| {
        record.content.normalized_body.as_deref() == Some("visible split response")
    }));
    assert!(records
        .iter()
        .any(|record| record.event_type == "tool_call"));
    let retained_core = serde_json::to_string(&records).unwrap();
    assert!(!retained_core.contains(PRIVATE_THINKING));
    assert!(!retained_core.contains(PRIVATE_SIGNATURE));
}
