use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::Command,
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{CertifiedSource, EventOrigin, SessionRelationshipKind};
use ctx_history_index::{GenerationWriter, RevalidationTarget, WriterOptions};

use super::*;
use crate::provider::codex::nativepath::{
    install_after_codex_causal_stage_hook_v1, install_after_codex_metadata_inventory_hook,
    CodexCausalSourceObservationV1,
};
use crate::provider::source_backed::family::jsonl::{
    set_after_jsonl_append_observation_route_binding_hook,
    set_before_jsonl_terminal_physical_revalidation_hook,
};

const CURRENT_PARSER_REVISION: &str =
    "codex-nativepath-core-record-v31-repository-positive-exact-authority";
const CURRENT_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v18";

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn session_path(root: &Path, native_session_id: &str) -> PathBuf {
    root.join(format!("rollout-{native_session_id}.jsonl"))
}

fn jsonl_bytes(records: impl IntoIterator<Item = serde_json::Value>) -> Vec<u8> {
    records
        .into_iter()
        .flat_map(|record| {
            let mut line = serde_json::to_vec(&record).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

fn session_meta(
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
) -> serde_json::Value {
    let source = match (relationship, parent_native_session_id) {
        (SessionRelationshipKind::Delegated, Some(parent)) => serde_json::json!({
            "subagent": {"thread_spawn": {"parent_thread_id": parent}}
        }),
        _ => serde_json::json!("cli"),
    };
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "session_id": native_session_id,
        "timestamp": "2026-08-09T12:00:00Z",
        "cwd": "/tmp/codex-child-independence",
        "originator": "codex_cli_rs",
        "cli_version": "0.1.0",
        "source": source,
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        match relationship {
            SessionRelationshipKind::Delegated => {
                payload["parent_thread_id"] = serde_json::json!(parent);
            }
            SessionRelationshipKind::Forked => {
                payload["forked_from_id"] = serde_json::json!(parent);
            }
            SessionRelationshipKind::ResumedFrom => {
                payload["history_base"] = serde_json::json!({
                    "thread_id": parent,
                    "end_ordinal_exclusive": 3,
                    "end_byte_offset": 512
                });
            }
            relationship => panic!("unsupported fixture relationship {relationship:?}"),
        }
    }
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:00Z",
        "type": "session_meta",
        "payload": payload
    })
}

fn message(marker: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": marker}]
        }
    })
}

fn turn_context() -> serde_json::Value {
    turn_context_with_id("019fb100-0000-7000-8000-000000000001")
}

fn turn_context_with_id(turn_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:02Z",
        "type": "turn_context",
        "payload": {
            "turn_id": turn_id,
            "cwd": "/tmp/codex-child-independence"
        }
    })
}

fn exec_call(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": "git rev-parse HEAD",
                "workdir": "/tmp/codex-child-independence",
                "yield_time_ms": 10000
            }).to_string()
        }
    })
}

fn unrelated_tool_call(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "unrelated_display_tool",
            "call_id": call_id,
            "arguments": "{}"
        }
    })
}

fn exec_result(call_id: &str, marker: &str) -> serde_json::Value {
    successful_result(
        call_id,
        format!("{marker}\n0123456789abcdef0123456789abcdef01234567\n"),
    )
}

fn successful_result(call_id: &str, output: String) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!(
                "Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 0\nFinal output:\n{output}"
            )
        }
    })
}

fn exec_call_in(call_id: &str, command: &str, workdir: &Path) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": workdir,
                "yield_time_ms": 10000
            }).to_string()
        }
    })
}

fn running_result(call_id: &str, cell_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!("Script running with cell ID {cell_id}\n")
        }
    })
}

fn wait_call(call_id: &str, cell_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:05Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "wait",
            "call_id": call_id,
            "arguments": serde_json::json!({"cell_id": cell_id}).to_string()
        }
    })
}

fn completed_wait_result(call_id: &str, output: impl AsRef<str>) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:06Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!(
                "Script completed\nProcess exited with code 0\nFinal output:\n{}",
                output.as_ref()
            )
        }
    })
}

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/codex-fixture.git",
        ],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success());
    }
    fs::write(path.join("tracked.txt"), "tracked\n").unwrap();
    for args in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "fixture"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success());
    }
}

fn write_session(
    root: &Path,
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let records = std::iter::once(session_meta(
        native_session_id,
        relationship,
        parent_native_session_id,
    ))
    .chain(events);
    fs::write(session_path(root, native_session_id), jsonl_bytes(records)).unwrap();
}

fn write_session_with_advisory(
    root: &Path,
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
    advisory_session_id: &str,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let mut meta = session_meta(native_session_id, relationship, parent_native_session_id);
    meta["payload"]["session_id"] = serde_json::json!(advisory_session_id);
    let records = std::iter::once(meta).chain(events);
    fs::write(session_path(root, native_session_id), jsonl_bytes(records)).unwrap();
}

fn append_event(path: &Path, event: serde_json::Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&jsonl_bytes([event])).unwrap();
    file.sync_all().unwrap();
}

fn destructively_mutate_session(path: &Path, replacement: &Path, mutation: &str) {
    match mutation {
        "rewrite" => {
            let mut contents = fs::read(path).unwrap();
            let marker = b"lastgooduniquetoken";
            let start = contents
                .windows(marker.len())
                .position(|window| window == marker)
                .expect("rewrite marker is present");
            contents[start] = b'L';
            fs::write(path, contents).unwrap();
        }
        "truncate" => {
            let file = OpenOptions::new().write(true).open(path).unwrap();
            file.set_len(fs::metadata(path).unwrap().len() / 2).unwrap();
            file.sync_all().unwrap();
        }
        "replacement" => {
            fs::remove_file(path).unwrap();
            fs::rename(replacement, path).unwrap();
        }
        _ => unreachable!(),
    }
}

fn register_tree(roots: &[&Path]) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut registry,
        roots
            .iter()
            .map(|root| {
                fixture_provider_source_at(
                    CaptureProvider::Codex,
                    "codex_session_jsonl_tree",
                    ProviderImportSupport::Native,
                    *root,
                )
            })
            .collect(),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn add_explicit_route(registry: &mut SourceBackedProviderRegistry, path: &Path) {
    register_landed_source_backed_route(
        registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            path,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
}

fn route_identity(registry: &SourceBackedProviderRegistry, root: &Path) -> SourceRouteIdentity {
    registry
        .routes()
        .find(|route| route.source.path == root)
        .and_then(|route| route.route_identity.clone())
        .expect("registered Codex route has an identity")
}

fn certificate_for(index: &VerifiedIndex, native_session_id: &str) -> CertifiedSource {
    index
        .manifest()
        .sources
        .iter()
        .find(|certificate| {
            matches!(
                certificate.observation().source().anchor(),
                SourceAnchor::ProviderNative { key: TypedKey::Utf8(value), .. }
                    if value == native_session_id
            )
        })
        .cloned()
        .unwrap_or_else(|| panic!("missing certificate for {native_session_id}"))
}

fn records_for(index: &VerifiedIndex, native_session_id: &str) -> Vec<CoreRecord> {
    let certificate = certificate_for(index, native_session_id);
    let page = index
        .source_event_page(certificate.observation().source(), None, 256)
        .unwrap();
    assert!(page.next_cursor.is_none());
    let mut records = page
        .items
        .into_iter()
        .map(|item| {
            index
                .core_record_by_id(item.event_id.as_uuid())
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn assert_no_repository_causality(records: &[CoreRecord], markers: &[&str]) {
    for marker in markers {
        let matching = records
            .iter()
            .filter(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains(marker))
            })
            .collect::<Vec<_>>();
        assert!(!matching.is_empty(), "missing adversarial marker {marker}");
        assert!(matching.iter().all(|record| {
            record.event_origin == EventOrigin::Unknown
                && record.repository_vcs_observations.is_empty()
        }));
    }
}

fn assert_exact_commit_causality(records: &[CoreRecord], markers: &[&str]) {
    for marker in markers {
        let record = records
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains(marker))
            })
            .unwrap_or_else(|| panic!("missing exact marker {marker}"));
        assert_eq!(record.event_origin, EventOrigin::UniqueToSession);
        assert!(record
            .repository_vcs_observations
            .iter()
            .any(|observation| {
                matches!(
                    observation.kind,
                    ctx_history_core::RepositoryVcsObservationKind::Outcome(ref outcome)
                        if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                )
            }));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSnapshot {
    certificate: Vec<u8>,
    records: Vec<Vec<u8>>,
    search_event_ids: Vec<String>,
}

fn source_snapshot(
    index: &VerifiedIndex,
    native_session_id: &str,
    search_marker: &str,
) -> SourceSnapshot {
    let mut search_event_ids = index
        .search_event_candidates(search_marker, 32)
        .unwrap()
        .into_iter()
        .filter(|candidate| {
            candidate.event.provider_session_id.as_deref() == Some(native_session_id)
        })
        .map(|candidate| candidate.event.event_id.to_string())
        .collect::<Vec<_>>();
    search_event_ids.sort();
    SourceSnapshot {
        certificate: serde_json::to_vec(&certificate_for(index, native_session_id)).unwrap(),
        records: records_for(index, native_session_id)
            .into_iter()
            .map(|record| serde_json::to_vec(&record).unwrap())
            .collect(),
        search_event_ids,
    }
}

fn capture_causal_stage() -> Arc<Mutex<Option<Vec<CodexCausalSourceObservationV1>>>> {
    let observed = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&observed);
    install_after_codex_causal_stage_hook_v1(move |sources| {
        *captured.lock().unwrap() = Some(sources);
    });
    observed
}

fn causal_by_id(
    observed: &Arc<Mutex<Option<Vec<CodexCausalSourceObservationV1>>>>,
) -> BTreeMap<String, CodexCausalSourceObservationV1> {
    observed
        .lock()
        .unwrap()
        .take()
        .expect("Codex causal stage hook did not run")
        .into_iter()
        .map(|source| (source.provider_session_id.clone(), source))
        .collect()
}

fn assert_exact_zero_work(
    sources: &BTreeMap<String, CodexCausalSourceObservationV1>,
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
) {
    let source = sources
        .get(native_session_id)
        .unwrap_or_else(|| panic!("missing causal source {native_session_id}"));
    assert_eq!(
        source.parent_provider_session_id.as_deref(),
        parent_native_session_id
    );
    let counters = source.counters;
    assert_eq!(counters.catalog_source_metadata_opens, 0);
    assert_eq!(counters.catalog_source_metadata_read_upper_bound_bytes, 0);
    assert_eq!(counters.catalog_session_meta_parses, 0);
    assert_eq!(counters.scanner_source_opens, 0);
    assert_eq!(counters.scanner_sources_started, 0);
    assert_eq!(counters.scanner_sources_completed, 0);
    assert_eq!(counters.scanner_bytes_read, 0);
    assert_eq!(counters.structural_json_parses, 0);
    assert_eq!(counters.typed_json_parses, 0);
    assert_eq!(counters.replaced_sources, 0);
    assert_eq!(counters.writer_mutated_sources, 0);
    assert_eq!(counters.staged_documents, 0);
    assert!(counters.writer_exact_replay_sources > 0);
}

fn refresh_and_assert_descendants(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    descendants: &[(&str, &str)],
) -> BTreeMap<String, CodexCausalSourceObservationV1> {
    let observed = capture_causal_stage();
    let receipt = refresh_source_backed_generation(index_root, registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    let sources = causal_by_id(&observed);
    for (descendant, parent) in descendants {
        assert_exact_zero_work(&sources, descendant, Some(parent));
    }
    sources
}

#[test]
fn parent_lifecycle_never_opens_scans_or_replaces_unchanged_descendants() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000001";
    let child = "019fb000-0000-7000-8000-000000000002";
    let grandchild = "019fb000-0000-7000-8000-000000000003";
    let great_grandchild = "019fb000-0000-7000-8000-000000000004";
    let parent_path = session_path(&sessions, parent);

    write_session_with_advisory(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        parent,
        [message("child-stable-marker")],
    );
    write_session_with_advisory(
        &sessions,
        grandchild,
        SessionRelationshipKind::Delegated,
        Some(child),
        parent,
        [message("grandchild-stable-marker")],
    );
    write_session_with_advisory(
        &sessions,
        great_grandchild,
        SessionRelationshipKind::Delegated,
        Some(grandchild),
        parent,
        [message("great-grandchild-stable-marker")],
    );
    let registry = register_tree(&[&sessions]);
    let initial_receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial_receipt.failed_routes.is_empty());
    assert!(initial_receipt.logical_source_failures.is_empty());
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 3);
    let child_snapshot = source_snapshot(&initial, child, "child-stable-marker");
    let grandchild_snapshot = source_snapshot(&initial, grandchild, "grandchild-stable-marker");
    let great_grandchild_snapshot =
        source_snapshot(&initial, great_grandchild, "great-grandchild-stable-marker");
    let child_records = records_for(&initial, child);
    let grandchild_records = records_for(&initial, grandchild);
    let great_grandchild_records = records_for(&initial, great_grandchild);
    assert!(child_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(child)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    assert!(grandchild_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(grandchild)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    assert!(great_grandchild_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(great_grandchild)
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    drop(initial);
    let descendants = [
        (child, parent),
        (grandchild, child),
        (great_grandchild, grandchild),
    ];

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-initial-marker")],
    );
    let arrived = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(arrived.get(parent).unwrap().counters.cold_sources, 1);

    append_event(&parent_path, message("parent-append-marker"));
    let appended = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    let parent_append = &appended.get(parent).unwrap().counters;
    assert_eq!(parent_append.appended_sources, 1);
    assert_eq!(parent_append.scanner_sources_started, 1);

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message(&format!(
            "parent-rewrite-marker-{}",
            "x".repeat(1_024)
        ))],
    );
    let rewritten = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(rewritten.get(parent).unwrap().counters.replaced_sources, 1);

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-truncated")],
    );
    let truncated = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(truncated.get(parent).unwrap().counters.replaced_sources, 1);

    fs::remove_file(&parent_path).unwrap();
    let deleted = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert!(!deleted.contains_key(parent));

    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("parent-reappeared-marker")],
    );
    let reappeared = refresh_and_assert_descendants(&index_root, &registry, &descendants);
    assert_eq!(reappeared.get(parent).unwrap().counters.cold_sources, 1);

    let final_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        source_snapshot(&final_index, child, "child-stable-marker"),
        child_snapshot
    );
    assert_eq!(
        source_snapshot(&final_index, grandchild, "grandchild-stable-marker"),
        grandchild_snapshot
    );
    assert_eq!(
        source_snapshot(
            &final_index,
            great_grandchild,
            "great-grandchild-stable-marker"
        ),
        great_grandchild_snapshot
    );
}

#[test]
fn nested_root_advisory_is_admitted_and_changed_child_processes_only_itself() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root = "019fb000-0000-7000-8000-000000000005";
    let parent = "019fb000-0000-7000-8000-000000000006";
    let child = "019fb000-0000-7000-8000-000000000007";
    write_session(
        &sessions,
        root,
        SessionRelationshipKind::Root,
        None,
        [message("nestedrootuniquetokenaaa")],
    );
    write_session_with_advisory(
        &sessions,
        parent,
        SessionRelationshipKind::Delegated,
        Some(root),
        root,
        [message("nestedparentuniquetokenbbb")],
    );
    write_session_with_advisory(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        root,
        [message("nestedchildinitialuniquetokenccc")],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 3);
    assert_eq!(
        initial
            .search_event_candidates("nestedchildinitialuniquetokenccc", 8)
            .unwrap()
            .len(),
        1
    );
    let parent_session_id = records_for(&initial, parent)[0].session_id;
    let child_records = records_for(&initial, child);
    assert!(child_records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some(child)
            && record.parent_session_id == Some(parent_session_id)
            && record.root_session_id == parent_session_id
    }));
    drop(initial);

    write_session_with_advisory(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        root,
        [message("nestedchildrewrittenuniquetokenddd")],
    );
    let observed = capture_causal_stage();
    let rewritten =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(rewritten.failed_routes.is_empty());
    assert!(rewritten.logical_source_failures.is_empty());
    let sources = causal_by_id(&observed);
    assert_exact_zero_work(&sources, root, None);
    assert_exact_zero_work(&sources, parent, Some(root));
    let child_counters = sources.get(child).unwrap().counters;
    assert_eq!(child_counters.scanner_source_opens, 1);
    assert_eq!(child_counters.scanner_sources_started, 1);
    assert_eq!(child_counters.scanner_sources_completed, 1);
    assert!(child_counters.scanner_bytes_read > 0);
    assert!(child_counters.typed_json_parses > 0);
    assert_eq!(child_counters.replaced_sources, 1);
    assert_eq!(child_counters.writer_mutated_sources, 1);
    assert_eq!(
        sources
            .iter()
            .filter(|(_, source)| source.counters.scanner_sources_started != 0)
            .map(|(native_session_id, _)| native_session_id.as_str())
            .collect::<Vec<_>>(),
        vec![child]
    );

    let current = VerifiedIndex::open(&index_root).unwrap();
    assert!(current
        .search_event_candidates("nestedchildinitialuniquetokenccc", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        current
            .search_event_candidates("nestedchildrewrittenuniquetokenddd", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(records_for(&current, child).iter().any(|record| {
        record
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| body.contains("nestedchildrewrittenuniquetokenddd"))
    }));
}

#[test]
fn append_after_large_terminal_authority_prefix_scans_only_the_suffix() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000009";
    let path = session_path(&sessions, native_session_id);
    let mut events = (0..4_097)
        .map(|index| {
            exec_result(
                &format!("completed-prefix-call-{index}"),
                "completed-prefix-result",
            )
        })
        .collect::<Vec<_>>();
    events.push(message("large-prefix-seed"));
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        events,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&path, message("largeprefixappenduniquetoken"));
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    let counters = sources.get(native_session_id).unwrap().counters;
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    assert_eq!(counters.scanner_sources_started, 1);
    assert_eq!(counters.complete_records_scanned, 1);
    assert_eq!(counters.retained_records_scanned, 1);
    assert_eq!(counters.staged_documents, 1);
    assert!(counters.mcp_terminal_authority_bytes_read < 4 * 1024);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    let appended_event_ids = appended
        .search_event_candidates("largeprefixappenduniquetoken", 8)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(appended_event_ids.len(), 1);

    let cold_index_root = temp.path().join("cold-index");
    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&cold, native_session_id)).unwrap(),
        appended_certificate
    );
    assert_eq!(
        cold.search_event_candidates("largeprefixappenduniquetoken", 8)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        appended_event_ids
    );
}

#[test]
fn selected_routes_process_child_only_and_never_abort_for_unselected_descendants() {
    let temp = tempdir().unwrap();
    let parent_root = temp.path().join("parent-sessions");
    let child_root = temp.path().join("child-sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&parent_root).unwrap();
    fs::create_dir_all(&child_root).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000011";
    let child = "019fb000-0000-7000-8000-000000000012";
    let parent_path = session_path(&parent_root, parent);
    let child_path = session_path(&child_root, child);
    write_session(
        &parent_root,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("selected-parent-initial")],
    );
    write_session(
        &child_root,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("selected-child-initial")],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut registry, &parent_path);
    add_explicit_route(&mut registry, &child_path);
    let parent_route = route_identity(&registry, &parent_path);
    let child_route = route_identity(&registry, &child_path);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&child_path, message("child-only-selected-marker"));
    let child_observed = capture_causal_stage();
    refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [child_route.clone()],
    )
    .unwrap();
    let child_sources = causal_by_id(&child_observed);
    assert_eq!(child_sources.len(), 1);
    assert_eq!(
        child_sources.get(child).unwrap().counters.appended_sources,
        1
    );
    assert!(!child_sources.contains_key(parent));

    append_event(&parent_path, message("simultaneousparentuniquetoken"));
    append_event(&child_path, message("simultaneouschilduniquetoken"));
    let before_unselected_child = source_snapshot(
        &VerifiedIndex::open(&index_root).unwrap(),
        child,
        "child-only-selected-marker",
    );
    let parent_observed = capture_causal_stage();
    let selected_parent = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [parent_route],
    )
    .unwrap();
    assert!(selected_parent.failed_routes.is_empty());
    let parent_sources = causal_by_id(&parent_observed);
    assert_eq!(parent_sources.len(), 1);
    assert_eq!(
        parent_sources
            .get(parent)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    assert!(!parent_sources.contains_key(child));
    let after_parent = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        source_snapshot(&after_parent, child, "child-only-selected-marker"),
        before_unselected_child
    );
    assert!(after_parent
        .search_event_candidates("simultaneouschilduniquetoken", 8)
        .unwrap()
        .is_empty());

    let child_catchup = capture_causal_stage();
    refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [child_route],
    )
    .unwrap();
    let catchup_sources = causal_by_id(&child_catchup);
    assert_eq!(
        catchup_sources
            .get(child)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    let caught_up = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        caught_up
            .search_event_candidates("simultaneousparentuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        caught_up
            .search_event_candidates("simultaneouschilduniquetoken", 8)
            .unwrap()
            .len(),
        1
    );

    append_event(&parent_path, message("bothselectedparentuniquetoken"));
    append_event(&child_path, message("bothselectedchilduniquetoken"));
    let both_selected =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(both_selected.failed_routes.is_empty());
    let simultaneous = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        simultaneous
            .search_event_candidates("bothselectedparentuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        simultaneous
            .search_event_candidates("bothselectedchilduniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn fork_invocation_boundary_separates_copied_and_unique_exact_outcomes() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let parent = "019fb000-0000-7000-8000-000000000021";
    let child = "019fb000-0000-7000-8000-000000000022";
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call_in("copied-call", command, &repository),
            successful_result(
                "copied-call",
                format!("positive-copied-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            message("origin-parent-marker"),
        ],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Forked,
        Some(parent),
        [
            turn_context_with_id("019fa000-0000-7000-8000-000000000001"),
            exec_call_in("copied-call", command, &repository),
            turn_context(),
            successful_result(
                "copied-call",
                format!("positive-copied-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("local-call", command, &repository),
            successful_result(
                "local-call",
                format!("unique-local-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = VerifiedIndex::open(&index_root).unwrap();
    let child_snapshot = source_snapshot(&before, child, "result-marker");
    let records = records_for(&before, child);
    assert!(records.iter().all(|record| {
        record.session_relationship == SessionRelationshipKind::Forked
            && record.parent_session_id.is_some()
            && record.parent_session_id == Some(record.root_session_id)
    }));
    let copied = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("positive-copied-result-marker"))
        })
        .expect("copied result record");
    assert!(matches!(
        copied.event_origin,
        EventOrigin::CopiedFromAncestor { .. }
    ));
    assert!(copied.repository_vcs_observations.is_empty());
    assert!(copied.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("copied_provider_history_has_ancestor_execution")
    }));
    let local = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("unique-local-result-marker"))
        })
        .expect("local result record");
    assert_eq!(local.event_origin, EventOrigin::UniqueToSession);
    assert!(local
        .repository_vcs_observations
        .iter()
        .any(|observation| matches!(
            &observation.kind,
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                    && outcome.produced_object_ids.iter().any(|object_id| object_id.hex == oid)
        )));
    assert!(!local.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));

    append_event(
        &session_path(&sessions, parent),
        message("origin-parent-append"),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_exact_zero_work(&sources, child, Some(parent));
    assert_eq!(
        source_snapshot(
            &VerifiedIndex::open(&index_root).unwrap(),
            child,
            "result-marker"
        ),
        child_snapshot
    );

    append_event(
        &session_path(&sessions, child),
        message("child-append-after-restart-marker"),
    );
    let child_append = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let child_append_sources = causal_by_id(&child_append);
    assert_eq!(
        child_append_sources
            .get(child)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    let restarted_records = records_for(&VerifiedIndex::open(&index_root).unwrap(), child);
    let restarted_copied = restarted_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("positive-copied-result-marker"))
        })
        .unwrap();
    assert!(matches!(
        restarted_copied.event_origin,
        EventOrigin::CopiedFromAncestor { .. }
    ));
}

#[test]
fn root_owned_exact_commit_and_pr_203_share_certified_repository_origin() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    assert!(Command::new("git")
        .args([
            "remote",
            "set-url",
            "origin",
            "https://github.com/ctxrs/ctx.git",
        ])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let native_session_id = "019fb000-0000-7000-8000-000000000024";
    let pr_command = concat!(
        "git push -u origin codex/exact-repository-origin\n",
        "gh pr create --base main --head codex/exact-repository-origin ",
        "--title 'exact repository origin' --body 'exact repository origin'"
    );
    let unrelated_terminals = (0..257).map(|index| {
        serde_json::json!({
            "timestamp": "2026-08-09T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": format!("unrelated-terminal-{index}")
            }
        })
    });
    let exact_events = [
        exec_call_in(
            "root-commit",
            "git commit -m exact-root-commit && git rev-parse HEAD",
            &repository,
        ),
        successful_result(
            "root-commit",
            format!("[main abc1234] exact-root-commit\n{oid}\n"),
        ),
        exec_call_in("root-pr-203", pr_command, &repository),
        successful_result(
            "root-pr-203",
            "To https://github.com/ctxrs/ctx.git\nhttps://github.com/ctxrs/ctx/pull/203\n"
                .to_owned(),
        ),
    ];
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        unrelated_terminals.chain(exact_events),
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&verified, native_session_id);
    let commit = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("exact-root-commit"))
                && !record.repository_vcs_observations.is_empty()
        })
        .expect("root exact commit record");
    assert_eq!(commit.event_origin, EventOrigin::UniqueToSession);
    let commit_outcome = commit
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit =>
            {
                Some((observation, outcome.as_ref()))
            }
            _ => None,
        })
        .expect("certified exact commit outcome");
    assert!(commit_outcome
        .1
        .produced_object_ids
        .iter()
        .any(|object_id| object_id.hex == oid));

    let pull_request = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("https://github.com/ctxrs/ctx/pull/203"))
        })
        .expect("root PR #203 record");
    assert_eq!(pull_request.event_origin, EventOrigin::UniqueToSession);
    let (pr_observation, pr_outcome) = pull_request
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == ctx_history_core::RepositoryOutcomeKind::PullRequestCreated =>
            {
                Some((observation, outcome.as_ref()))
            }
            _ => None,
        })
        .expect("certified PR #203 creation outcome");
    let pr_identity = pr_outcome.pull_request.as_ref().expect("PR identity");
    assert_eq!(pr_identity.number, 203);
    let binding = pull_request
        .repository_bindings
        .iter()
        .find(|binding| binding.binding_id == pr_observation.repository_binding_id)
        .expect("PR #203 repository binding");
    assert!(binding.accepts_pull_request(pr_identity));
    assert_eq!(
        commit_outcome.0.repository_binding_id,
        pr_observation.repository_binding_id
    );
}

#[test]
fn duplicate_pre_turn_provider_identity_is_unknown_on_cold_and_append_restart() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000025";
    let child = "019fb000-0000-7000-8000-000000000026";
    let duplicated = [
        exec_call("duplicate-pre-turn-call"),
        exec_result("duplicate-pre-turn-call", "duplicate-pre-turn-first"),
        exec_call("duplicate-pre-turn-call"),
        exec_result("duplicate-pre-turn-call", "duplicate-pre-turn-second"),
    ];
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        duplicated.clone(),
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Forked,
        Some(parent),
        duplicated,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let cold = VerifiedIndex::open(&index_root).unwrap();
    let duplicate_records = records_for(&cold, child)
        .into_iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("duplicate-pre-turn"))
        })
        .collect::<Vec<_>>();
    assert_eq!(duplicate_records.len(), 2);
    assert!(duplicate_records
        .iter()
        .all(|record| record.event_origin == EventOrigin::Unknown));
    drop(cold);

    append_event(
        &session_path(&sessions, child),
        message("duplicate-provider-append-restart"),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_eq!(sources.get(child).unwrap().counters.appended_sources, 1);
    let appended = VerifiedIndex::open(&index_root).unwrap();
    assert!(records_for(&appended, child)
        .iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("duplicate-pre-turn"))
        })
        .all(|record| record.event_origin == EventOrigin::Unknown));
    let appended_certificate = serde_json::to_vec(&certificate_for(&appended, child)).unwrap();
    drop(appended);

    let cold_index_root = temp.path().join("cold-index");
    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let cold_restart = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&cold_restart, child)).unwrap(),
        appended_certificate
    );
}

#[test]
fn unmatched_or_ambiguous_call_ids_suppress_exact_outcomes() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let root = "019fb000-0000-7000-8000-000000000030";
    write_session(
        &sessions,
        root,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call_in("ambiguous-call", command, &repository),
            exec_call_in("ambiguous-call", command, &repository),
            successful_result(
                "ambiguous-call",
                format!("ambiguous-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("matched-call", command, &repository),
            successful_result(
                "mismatched-result-call",
                format!("mismatched-result-marker\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    let root_records = records_for(&verified, root);
    let ambiguous = root_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("ambiguous-result-marker"))
        })
        .expect("ambiguous result record");
    assert_eq!(ambiguous.event_origin, EventOrigin::Unknown);
    assert!(ambiguous.repository_vcs_observations.is_empty());
    let mismatched = root_records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("mismatched-result-marker"))
        })
        .expect("mismatched result record");
    assert_eq!(mismatched.event_origin, EventOrigin::Unknown);
    assert!(mismatched.repository_vcs_observations.is_empty());
}

#[test]
fn cold_direct_repository_results_require_exact_candidate_multiplicity() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000033";
    let mut events = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("direct-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    events.extend([
        successful_result(
            "direct-pre-result",
            format!("direct-pre-result-before\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-pre-result", command, &repository),
        successful_result(
            "direct-pre-result",
            format!("direct-pre-result-after\n[main abc1234] exact\n{oid}\n"),
        ),
        unrelated_tool_call("direct-pre-call"),
        exec_call_in("direct-pre-call", command, &repository),
        successful_result(
            "direct-pre-call",
            format!("direct-pre-call-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-duplicate", command, &repository),
        successful_result(
            "direct-duplicate",
            format!("direct-duplicate-same\n[main abc1234] exact\n{oid}\n"),
        ),
        successful_result(
            "direct-duplicate",
            format!("direct-duplicate-same\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-conflict", command, &repository),
        successful_result(
            "direct-conflict",
            format!("direct-conflict-first\n[main abc1234] exact\n{oid}\n"),
        ),
        successful_result(
            "direct-conflict",
            "direct-conflict-second\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n"
                .to_owned(),
        ),
        exec_call_in("direct-serial", command, &repository),
        successful_result(
            "direct-serial",
            format!("direct-serial-first\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("direct-serial", command, &repository),
        successful_result(
            "direct-serial",
            format!("direct-serial-second\n[main abc1234] exact\n{oid}\n"),
        ),
    ]);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        events,
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&verified, native_session_id),
        &[
            "direct-pre-result-before",
            "direct-pre-result-after",
            "direct-pre-call-after",
            "direct-duplicate-same",
            "direct-conflict-first",
            "direct-conflict-second",
            "direct-serial-first",
            "direct-serial-second",
        ],
    );
}

#[test]
fn cold_continued_repository_results_require_exact_candidate_multiplicity() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000034";
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            completed_wait_result(
                "continued-pre-result-wait",
                format!("continued-pre-result-before\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-pre-result-origin", command, &repository),
            running_result("continued-pre-result-origin", "continued-pre-result-cell"),
            wait_call("continued-pre-result-wait", "continued-pre-result-cell"),
            completed_wait_result(
                "continued-pre-result-wait",
                format!("continued-pre-result-after\n[main abc1234] exact\n{oid}\n"),
            ),
            unrelated_tool_call("continued-pre-call-wait"),
            exec_call_in("continued-pre-call-origin", command, &repository),
            running_result("continued-pre-call-origin", "continued-pre-call-cell"),
            wait_call("continued-pre-call-wait", "continued-pre-call-cell"),
            completed_wait_result(
                "continued-pre-call-wait",
                format!("continued-pre-call-after\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-duplicate-origin", command, &repository),
            running_result("continued-duplicate-origin", "continued-duplicate-cell"),
            wait_call("continued-duplicate-wait", "continued-duplicate-cell"),
            completed_wait_result(
                "continued-duplicate-wait",
                format!("continued-duplicate-same\n[main abc1234] exact\n{oid}\n"),
            ),
            completed_wait_result(
                "continued-duplicate-wait",
                format!("continued-duplicate-same\n[main abc1234] exact\n{oid}\n"),
            ),
            exec_call_in("continued-conflict-origin", command, &repository),
            running_result("continued-conflict-origin", "continued-conflict-cell"),
            wait_call("continued-conflict-wait", "continued-conflict-cell"),
            completed_wait_result(
                "continued-conflict-wait",
                format!("continued-conflict-first\n[main abc1234] exact\n{oid}\n"),
            ),
            completed_wait_result(
                "continued-conflict-wait",
                "continued-conflict-second\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n",
            ),
            exec_call_in("continued-serial-origin", command, &repository),
            running_result("continued-serial-origin", "continued-serial-cell"),
            wait_call("continued-serial-wait", "continued-serial-cell"),
            completed_wait_result(
                "continued-serial-wait",
                format!("continued-serial-first\n[main abc1234] exact\n{oid}\n"),
            ),
            wait_call("continued-serial-wait", "continued-serial-cell"),
            completed_wait_result(
                "continued-serial-wait",
                format!("continued-serial-second\n[main abc1234] exact\n{oid}\n"),
            ),
        ],
    );

    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&verified, native_session_id),
        &[
            "continued-pre-result-before",
            "continued-pre-result-after",
            "continued-pre-call-after",
            "continued-duplicate-same",
            "continued-conflict-first",
            "continued-conflict-second",
            "continued-serial-first",
            "continued-serial-second",
        ],
    );
}

#[test]
fn append_restart_counts_candidate_id_occurrences_before_first_admission() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let cold_index_root = temp.path().join("cold-index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000036";
    let path = session_path(&sessions, native_session_id);
    let mut prefix = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("late-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    prefix.extend([
        successful_result(
            "late-direct-result",
            format!("late-direct-before\n[main abc1234] exact\n{oid}\n"),
        ),
        unrelated_tool_call("late-direct-call"),
        completed_wait_result(
            "late-continued-wait",
            format!("late-continued-before\n[main abc1234] exact\n{oid}\n"),
        ),
    ]);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        prefix,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    for event in [
        exec_call_in("late-direct-result", command, &repository),
        successful_result(
            "late-direct-result",
            format!("late-direct-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("late-direct-call", command, &repository),
        successful_result(
            "late-direct-call",
            format!("late-call-after\n[main abc1234] exact\n{oid}\n"),
        ),
        exec_call_in("late-continued-origin", command, &repository),
        running_result("late-continued-origin", "late-continued-cell"),
        wait_call("late-continued-wait", "late-continued-cell"),
        completed_wait_result(
            "late-continued-wait",
            format!("late-continued-after\n[main abc1234] exact\n{oid}\n"),
        ),
    ] {
        append_event(&path, event);
    }

    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&observed)
            .get(native_session_id)
            .unwrap()
            .counters
            .appended_sources,
        1
    );
    let appended = VerifiedIndex::open(&index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&appended, native_session_id),
        &[
            "late-direct-before",
            "late-direct-after",
            "late-call-after",
            "late-continued-before",
            "late-continued-after",
        ],
    );
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    drop(appended);

    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let restarted = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&restarted, native_session_id),
        &[
            "late-direct-after",
            "late-call-after",
            "late-continued-after",
        ],
    );
    assert_eq!(
        serde_json::to_vec(&certificate_for(&restarted, native_session_id)).unwrap(),
        appended_certificate
    );
}

#[test]
fn append_restart_retracts_direct_and_continued_candidate_reuse_after_large_prefix() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let cold_index_root = temp.path().join("cold-index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let command = "git commit -m exact && git rev-parse HEAD";
    let native_session_id = "019fb000-0000-7000-8000-000000000035";
    let path = session_path(&sessions, native_session_id);
    let mut events = (0..300)
        .map(|index| {
            serde_json::json!({
                "timestamp": "2026-08-09T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("append-unrelated-prefix-{index}")
                }
            })
        })
        .collect::<Vec<_>>();
    for kind in ["duplicate", "conflict", "serial"] {
        let call_id = format!("append-direct-{kind}");
        events.push(exec_call_in(&call_id, command, &repository));
        events.push(successful_result(
            &call_id,
            format!("append-direct-{kind}-initial\n[main abc1234] exact\n{oid}\n"),
        ));
    }
    for kind in ["duplicate", "conflict", "serial"] {
        let origin = format!("append-continued-{kind}-origin");
        let cell = format!("append-continued-{kind}-cell");
        let wait = format!("append-continued-{kind}-wait");
        events.push(exec_call_in(&origin, command, &repository));
        events.push(running_result(&origin, &cell));
        events.push(wait_call(&wait, &cell));
        events.push(completed_wait_result(
            &wait,
            format!("append-continued-{kind}-initial\n[main abc1234] exact\n{oid}\n"),
        ));
    }
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        events,
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_exact_commit_causality(
        &records_for(&initial, native_session_id),
        &[
            "append-direct-duplicate-initial",
            "append-direct-conflict-initial",
            "append-direct-serial-initial",
            "append-continued-duplicate-initial",
            "append-continued-conflict-initial",
            "append-continued-serial-initial",
        ],
    );
    drop(initial);

    append_event(
        &path,
        successful_result(
            "append-direct-duplicate",
            format!("append-direct-duplicate-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        successful_result(
            "append-direct-conflict",
            "append-direct-conflict-again\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n"
                .to_owned(),
        ),
    );
    append_event(
        &path,
        exec_call_in("append-direct-serial", command, &repository),
    );
    append_event(
        &path,
        successful_result(
            "append-direct-serial",
            format!("append-direct-serial-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-duplicate-wait",
            format!("append-continued-duplicate-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-conflict-wait",
            "append-continued-conflict-again\n[main fffffff] conflict\nffffffffffffffffffffffffffffffffffffffff\n",
        ),
    );
    append_event(
        &path,
        wait_call(
            "append-continued-serial-wait",
            "append-continued-serial-cell",
        ),
    );
    append_event(
        &path,
        completed_wait_result(
            "append-continued-serial-wait",
            format!("append-continued-serial-again\n[main abc1234] exact\n{oid}\n"),
        ),
    );

    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&observed)
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let appended = VerifiedIndex::open(&index_root).unwrap();
    let appended_records = records_for(&appended, native_session_id);
    assert_no_repository_causality(
        &appended_records,
        &[
            "append-direct-duplicate-initial",
            "append-direct-duplicate-again",
            "append-direct-conflict-initial",
            "append-direct-conflict-again",
            "append-direct-serial-initial",
            "append-direct-serial-again",
            "append-continued-duplicate-initial",
            "append-continued-duplicate-again",
            "append-continued-conflict-initial",
            "append-continued-conflict-again",
            "append-continued-serial-initial",
            "append-continued-serial-again",
        ],
    );
    let appended_certificate =
        serde_json::to_vec(&certificate_for(&appended, native_session_id)).unwrap();
    drop(appended);

    refresh_source_backed_generation(&cold_index_root, &registry, writer_options()).unwrap();
    let restarted = VerifiedIndex::open(&cold_index_root).unwrap();
    assert_no_repository_causality(
        &records_for(&restarted, native_session_id),
        &[
            "append-direct-duplicate-initial",
            "append-direct-conflict-initial",
            "append-direct-serial-initial",
            "append-continued-duplicate-initial",
            "append-continued-conflict-initial",
            "append-continued-serial-initial",
        ],
    );
    assert_eq!(
        serde_json::to_vec(&certificate_for(&restarted, native_session_id)).unwrap(),
        appended_certificate
    );
}

#[test]
fn fallback_identity_is_rewrite_stable_and_duplicate_occurrences_remain_distinct() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000027";
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("fallback-stable-first"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-last"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let initial = VerifiedIndex::open(&index_root).unwrap();
    let initial_records = records_for(&initial, native_session_id);
    let initial_duplicates = initial_records
        .iter()
        .filter(|record| {
            record.content.normalized_body.as_deref() == Some("fallback-stable-duplicate")
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(initial_duplicates.len(), 2);
    assert_ne!(initial_duplicates[0], initial_duplicates[1]);
    let initial_stable = initial_records
        .iter()
        .filter(|record| {
            matches!(
                record.content.normalized_body.as_deref(),
                Some("fallback-stable-first" | "fallback-stable-last")
            )
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    drop(initial);

    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("fallback-inserted-before"),
            message("fallback-stable-first"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-duplicate"),
            message("fallback-stable-last"),
        ],
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    assert_eq!(
        sources
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    let rewritten_records = records_for(&rewritten, native_session_id);
    let rewritten_duplicates = rewritten_records
        .iter()
        .filter(|record| {
            record.content.normalized_body.as_deref() == Some("fallback-stable-duplicate")
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    let rewritten_stable = rewritten_records
        .iter()
        .filter(|record| {
            matches!(
                record.content.normalized_body.as_deref(),
                Some("fallback-stable-first" | "fallback-stable-last")
            )
        })
        .map(|record| (record.event_id, record.native_event_id.clone().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(rewritten_duplicates, initial_duplicates);
    assert_eq!(rewritten_stable, initial_stable);
}

#[test]
fn direct_source_rewrite_delete_and_reappearance_replace_only_that_source() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000028";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            message("oldsourceuniquetoken"),
            message("staledocumentuniquetoken"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("newsourceuniquetoken")],
    );
    let replacement = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&replacement)
            .get(native_session_id)
            .unwrap()
            .counters
            .replaced_sources,
        1
    );
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    assert!(rewritten
        .search_event_candidates("staledocumentuniquetoken", 8)
        .unwrap()
        .is_empty());
    assert_eq!(
        rewritten
            .search_event_candidates("newsourceuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
    drop(rewritten);

    fs::remove_file(&path).unwrap();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let deleted = VerifiedIndex::open(&index_root).unwrap();
    assert!(deleted.manifest().sources.is_empty());
    assert!(deleted
        .search_event_candidates("newsourceuniquetoken", 8)
        .unwrap()
        .is_empty());
    drop(deleted);

    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [message("reappearedsourceuniquetoken")],
    );
    let reappeared = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&reappeared)
            .get(native_session_id)
            .unwrap()
            .counters
            .cold_sources,
        1
    );
}

#[test]
fn continuation_restart_preserves_exact_result_linkage_and_origin_proof() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fb000-0000-7000-8000-000000000029";
    let path = session_path(&sessions, native_session_id);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call_in(
                "continuation-origin",
                "git commit -m exact && git rev-parse HEAD",
                &repository,
            ),
            running_result("continuation-origin", "cell-exact-7"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&path, wait_call("continuation-wait", "cell-exact-7"));
    append_event(
        &path,
        serde_json::json!({
            "timestamp": "2026-08-09T12:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "continuation-wait",
                "status": "success",
                "output": format!(
                    "Script completed\nProcess exited with code 0\nFinal output:\n[main abc1234] exact\n{oid}\n"
                )
            }
        }),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&observed)
            .get(native_session_id)
            .unwrap()
            .counters
            .appended_sources,
        1
    );

    let verified = VerifiedIndex::open(&index_root).unwrap();
    let result = records_for(&verified, native_session_id)
        .into_iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(oid))
        })
        .unwrap();
    assert_eq!(result.event_origin, EventOrigin::UniqueToSession);
    assert!(result
        .repository_vcs_observations
        .iter()
        .any(|observation| {
            matches!(
                &observation.kind,
                ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                    if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                        && outcome
                            .produced_object_ids
                            .iter()
                            .any(|object_id| object_id.hex == oid)
            )
        }));
    assert!(!result.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));
    let activity = result
        .content
        .structured_content
        .as_ref()
        .and_then(|content| content.get("provider_native_tool_activities"))
        .and_then(serde_json::Value::as_array)
        .and_then(|activities| activities.first())
        .and_then(|activity| activity.get("provider_native_tool_result"))
        .unwrap();
    assert_eq!(
        activity
            .get("origin_call_id")
            .and_then(serde_json::Value::as_str),
        Some("continuation-origin")
    );
    assert_eq!(
        activity
            .get("result_call_id")
            .and_then(serde_json::Value::as_str),
        Some("continuation-wait")
    );
    assert_eq!(
        activity
            .get("continuation_call_id_sha256")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        activity
            .get("captured_outcomes")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
}

#[test]
fn missing_parent_local_continuation_restart_retains_exact_commit_origin() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    let repository = temp.path().join("repository");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let missing_parent = "019fb000-0000-7000-8000-000000000028";
    let native_session_id = "019fb000-0000-7000-8000-000000000029";
    let path = session_path(&sessions, native_session_id);
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Forked,
        Some(missing_parent),
        [
            turn_context(),
            exec_call_in(
                "continuation-origin",
                "git commit -m exact && git rev-parse HEAD",
                &repository,
            ),
            running_result("continuation-origin", "cell-exact-7"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&path, wait_call("continuation-wait", "cell-exact-7"));
    append_event(
        &path,
        serde_json::json!({
            "timestamp": "2026-08-09T12:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "continuation-wait",
                "status": "success",
                "output": format!(
                    "Script completed\nProcess exited with code 0\nFinal output:\n[main abc1234] exact\n{oid}\n"
                )
            }
        }),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        causal_by_id(&observed)
            .get(native_session_id)
            .unwrap()
            .counters
            .appended_sources,
        1
    );

    let verified = VerifiedIndex::open(&index_root).unwrap();
    let result = records_for(&verified, native_session_id)
        .into_iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(oid))
        })
        .unwrap();
    assert_eq!(result.parent_session_id, Some(result.root_session_id));
    assert_eq!(result.event_origin, EventOrigin::UniqueToSession);
    assert!(result
        .repository_vcs_observations
        .iter()
        .any(|observation| {
            matches!(
                &observation.kind,
                ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                    if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                        && outcome
                            .produced_object_ids
                            .iter()
                            .any(|object_id| object_id.hex == oid)
            )
        }));
    assert!(!result.repository_abstentions.iter().any(|abstention| {
        abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));
    let activity = result
        .content
        .structured_content
        .as_ref()
        .and_then(|content| content.get("provider_native_tool_activities"))
        .and_then(serde_json::Value::as_array)
        .and_then(|activities| activities.first())
        .and_then(|activity| activity.get("provider_native_tool_result"))
        .unwrap();
    assert_eq!(
        activity
            .get("origin_call_id")
            .and_then(serde_json::Value::as_str),
        Some("continuation-origin")
    );
    assert_eq!(
        activity
            .get("result_call_id")
            .and_then(serde_json::Value::as_str),
        Some("continuation-wait")
    );
    assert_eq!(
        activity
            .get("continuation_call_id_sha256")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        activity
            .get("captured_outcomes")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
}

#[test]
fn parser_revision_migration_rescans_each_source_once_without_legacy_decode() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000031";
    let child = "019fb000-0000-7000-8000-000000000032";
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("migration-parent-marker")],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("migration-child-marker")],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let current = VerifiedIndex::open(&index_root).unwrap();
    let old_routes = current.manifest().source_routes().to_vec();
    let old_sources = current
        .manifest()
        .sources
        .iter()
        .map(|certificate| {
            let native_session_id = match certificate.observation().source().anchor() {
                SourceAnchor::ProviderNative {
                    key: TypedKey::Utf8(value),
                    ..
                } => value,
                anchor => panic!("unexpected Codex source anchor {anchor:?}"),
            };
            let old_certificate = CertifiedSource::certify_with_frontier(
                certificate.observation().clone(),
                certificate.observation().clone(),
                "codex-nativepath-core-record-v27-bounded-exact-origin",
                *certificate.content_digest(),
                certificate.counts(),
                certificate.frontier().cloned(),
            )
            .unwrap();
            (old_certificate, records_for(&current, native_session_id))
        })
        .collect::<Vec<_>>();
    drop(current);
    let mut downgrade = GenerationWriter::open(&index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    downgrade
        .set_source_route_plan(
            old_routes
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        )
        .unwrap();
    for route in &old_routes {
        downgrade
            .begin_source_route_stage(route.route_identity().clone())
            .unwrap();
        for source in route.sources() {
            let (certificate, records) = old_sources
                .iter()
                .find(|(certificate, _)| {
                    certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(source)
                })
                .expect("route source has a retired-revision candidate");
            downgrade
                .begin_source(certificate.observation().source().clone())
                .unwrap();
            for record in records {
                downgrade.add_core_record(record.clone()).unwrap();
            }
            downgrade.certify_source(certificate.clone()).unwrap();
        }
        downgrade
            .finish_source_route_stage(route.route_identity())
            .unwrap();
    }
    downgrade.set_present_source_routes(old_routes).unwrap();
    downgrade
        .commit(|target| match target {
            RevalidationTarget::Source(actual) => {
                old_sources.iter().any(|(expected, _)| expected == actual)
            }
            RevalidationTarget::Deletion(_) => false,
        })
        .unwrap();

    let observed = capture_causal_stage();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let sources = causal_by_id(&observed);
    for native_session_id in [parent, child] {
        let counters = sources.get(native_session_id).unwrap().counters;
        assert_eq!(counters.catalog_source_metadata_opens, 1);
        assert!(counters.catalog_source_metadata_read_upper_bound_bytes > 0);
        assert_eq!(counters.catalog_session_meta_parses, 1);
        assert_eq!(counters.scanner_source_opens, 1);
        assert_eq!(counters.scanner_sources_started, 1);
        assert_eq!(counters.scanner_sources_completed, 1);
        assert_eq!(counters.replaced_sources, 1);
        assert_eq!(counters.writer_mutated_sources, 1);
    }
    let migrated = VerifiedIndex::open(&index_root).unwrap();
    for certificate in &migrated.manifest().sources {
        assert_eq!(certificate.parser_revision(), CURRENT_PARSER_REVISION);
        let frontier = certificate.frontier().unwrap();
        assert_eq!(frontier.checkpoint_kind(), CURRENT_FRONTIER_KIND);
        let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
            panic!("Codex checkpoint must be byte keyed");
        };
        let wire = serde_json::from_slice::<serde_json::Value>(bytes).unwrap();
        assert_eq!(wire["version"], 18);
        assert!(wire.get("certified_lineage_facts").is_none());
        assert!(wire.get("dependency_digest").is_none());
    }
}

#[test]
fn cold_continuous_appends_during_frozen_prefix_admission_catch_up_once() {
    const GENERATION_APPEND_MARKER: &str = "coldgenerationappendtoken306a";
    const TERMINAL_APPEND_MARKER: &str = "coldterminalappendtoken306b";
    const PRECOMMIT_APPEND_MARKER: &str = "coldprecommitappendtoken306c";

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000041";
    let child = "019fb000-0000-7000-8000-000000000042";
    let parent_path = session_path(&sessions, parent);
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [message("coldprefixuniquetoken")],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Delegated,
        Some(parent),
        [message("coldchildstableuniquetoken")],
    );
    let registry = register_tree(&[&sessions]);

    let generation_observation = Arc::new(Barrier::new(2));
    let terminal_observation = Arc::new(Barrier::new(2));
    let precommit_physical_revalidation = Arc::new(Barrier::new(2));
    let writer_path = parent_path.clone();
    let writer_generation_observation = Arc::clone(&generation_observation);
    let writer_terminal_observation = Arc::clone(&terminal_observation);
    let writer_precommit_physical_revalidation = Arc::clone(&precommit_physical_revalidation);
    let writer = std::thread::spawn(move || {
        writer_generation_observation.wait();
        append_event(&writer_path, message(GENERATION_APPEND_MARKER));
        writer_generation_observation.wait();

        writer_terminal_observation.wait();
        append_event(&writer_path, message(TERMINAL_APPEND_MARKER));
        writer_terminal_observation.wait();

        writer_precommit_physical_revalidation.wait();
        append_event(&writer_path, message(PRECOMMIT_APPEND_MARKER));
        writer_precommit_physical_revalidation.wait();
    });

    let generation_hook = Arc::clone(&generation_observation);
    let terminal_hook_path = parent_path.clone();
    let terminal_hook = Arc::clone(&terminal_observation);
    set_after_jsonl_append_observation_route_binding_hook(parent_path.clone(), move || {
        generation_hook.wait();
        generation_hook.wait();
        set_after_jsonl_append_observation_route_binding_hook(terminal_hook_path, move || {
            terminal_hook.wait();
            terminal_hook.wait();
        });
    });
    let precommit_hook = Arc::clone(&precommit_physical_revalidation);
    set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
        precommit_hook.wait();
        precommit_hook.wait();
    });

    let cold_causal = capture_causal_stage();
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    writer.join().expect("bounded Codex appender completed");
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    let cold_sources = causal_by_id(&cold_causal);
    assert_eq!(cold_sources.get(parent).unwrap().counters.cold_sources, 1);
    assert_eq!(cold_sources.get(child).unwrap().counters.cold_sources, 1);

    let initial = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(initial.manifest().sources.len(), 2);
    let cold_generation = initial.generation_id().to_owned();
    let cold_parent = source_snapshot(&initial, parent, "coldprefixuniquetoken");
    let cold_child = source_snapshot(&initial, child, "coldchildstableuniquetoken");
    assert_eq!(cold_parent.search_event_ids.len(), 1);
    assert_eq!(records_for(&initial, parent).len(), 1);
    for marker in [
        GENERATION_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert!(
            initial
                .search_event_candidates(marker, 8)
                .unwrap()
                .is_empty(),
            "cold publication included deferred suffix {marker}"
        );
    }
    drop(initial);

    let catch_up_causal = capture_causal_stage();
    let caught_up =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(caught_up.failed_routes.is_empty());
    assert!(caught_up.logical_source_failures.is_empty());
    let catch_up_sources = causal_by_id(&catch_up_causal);
    let parent_counters = catch_up_sources.get(parent).unwrap().counters;
    assert_eq!(parent_counters.appended_sources, 1);
    assert_eq!(parent_counters.replaced_sources, 0);
    assert_eq!(parent_counters.scanner_sources_started, 1);
    assert_eq!(parent_counters.scanner_sources_completed, 1);
    assert_eq!(parent_counters.complete_records_scanned, 3);
    assert_eq!(parent_counters.retained_records_scanned, 3);
    assert_eq!(parent_counters.staged_documents, 3);
    assert_exact_zero_work(&catch_up_sources, child, Some(parent));

    let current = VerifiedIndex::open(&index_root).unwrap();
    assert_ne!(current.generation_id(), cold_generation);
    let caught_up_generation = current.generation_id().to_owned();
    let caught_up_parent = source_snapshot(&current, parent, "coldprefixuniquetoken");
    assert_eq!(records_for(&current, parent).len(), 4);
    for marker in [
        GENERATION_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert_eq!(
            source_snapshot(&current, parent, marker)
                .search_event_ids
                .len(),
            1,
            "catch-up did not index suffix {marker} exactly once"
        );
    }
    assert_eq!(
        source_snapshot(&current, child, "coldchildstableuniquetoken"),
        cold_child
    );
    drop(current);

    let no_op_causal = capture_causal_stage();
    let no_op = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(no_op.failed_routes.is_empty());
    assert!(no_op.logical_source_failures.is_empty());
    let no_op_sources = causal_by_id(&no_op_causal);
    assert_exact_zero_work(&no_op_sources, parent, None);
    assert_exact_zero_work(&no_op_sources, child, Some(parent));

    let terminal = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(terminal.generation_id(), caught_up_generation);
    assert_eq!(
        source_snapshot(&terminal, parent, "coldprefixuniquetoken"),
        caught_up_parent
    );
    for marker in [
        GENERATION_APPEND_MARKER,
        TERMINAL_APPEND_MARKER,
        PRECOMMIT_APPEND_MARKER,
    ] {
        assert_eq!(
            source_snapshot(&terminal, parent, marker)
                .search_event_ids
                .len(),
            1,
            "terminal no-op changed suffix {marker}"
        );
    }
    assert_eq!(
        source_snapshot(&terminal, child, "coldchildstableuniquetoken"),
        cold_child
    );
}

#[test]
fn destructive_mid_capture_changes_preserve_last_good_generation_atomically() {
    for seam in [
        "metadata_inventory",
        "generation_observation",
        "terminal_observation",
        "precommit_physical_revalidation",
    ] {
        for mutation in ["rewrite", "truncate", "replacement"] {
            let temp = tempdir().unwrap();
            let sessions = temp.path().join("sessions");
            let index_root = temp.path().join("index");
            fs::create_dir_all(&sessions).unwrap();
            let native_session_id = "019fb000-0000-7000-8000-000000000041";
            let path = session_path(&sessions, native_session_id);
            write_session(
                &sessions,
                native_session_id,
                SessionRelationshipKind::Root,
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
                        session_meta(native_session_id, SessionRelationshipKind::Root, None),
                        message("replacementuniquetoken"),
                    ]),
                )
                .unwrap();
            }
            match seam {
                "metadata_inventory" => {
                    install_after_codex_metadata_inventory_hook(move || {
                        destructively_mutate_session(&mutate, &replacement, mutation);
                    });
                }
                "generation_observation" => {
                    set_after_jsonl_append_observation_route_binding_hook(
                        path.clone(),
                        move || {
                            destructively_mutate_session(&mutate, &replacement, mutation);
                        },
                    );
                }
                "terminal_observation" => {
                    let terminal_hook_path = path.clone();
                    set_after_jsonl_append_observation_route_binding_hook(
                        path.clone(),
                        move || {
                            set_after_jsonl_append_observation_route_binding_hook(
                                terminal_hook_path,
                                move || {
                                    destructively_mutate_session(&mutate, &replacement, mutation);
                                },
                            );
                        },
                    );
                }
                "precommit_physical_revalidation" => {
                    set_before_jsonl_terminal_physical_revalidation_hook(
                        sessions.clone(),
                        move || {
                            destructively_mutate_session(&mutate, &replacement, mutation);
                        },
                    );
                }
                _ => unreachable!(),
            }
            match refresh_source_backed_generation(&index_root, &registry, writer_options()) {
                Ok(failed) => {
                    assert_eq!(failed.failed_routes.len(), 1, "{seam}/{mutation}");
                    assert!(failed.failed_routes[0].carried_forward, "{seam}/{mutation}");
                }
                Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
                    assert_eq!(
                        source.kind,
                        SourceBackedRouteErrorKind::InvalidSource,
                        "{seam}/{mutation}"
                    );
                }
                Err(error) => panic!("unexpected {seam}/{mutation} failure: {error:?}"),
            }
            let retained = VerifiedIndex::open(&index_root).unwrap();
            assert_eq!(retained.generation_id(), generation, "{seam}/{mutation}");
            assert_eq!(
                source_snapshot(&retained, native_session_id, "lastgooduniquetoken"),
                snapshot,
                "{seam}/{mutation}"
            );
            assert!(retained
                .search_event_candidates("replacementuniquetoken", 8)
                .unwrap()
                .is_empty());
        }
    }
}
