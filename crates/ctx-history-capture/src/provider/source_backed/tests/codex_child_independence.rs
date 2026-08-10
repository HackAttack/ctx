use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

use ctx_history_core::{CertifiedSource, EventOrigin, SessionRelationshipKind};
use ctx_history_index::{GenerationWriter, RevalidationTarget, WriterOptions};

use super::*;
use crate::provider::codex::nativepath::{
    install_after_codex_causal_stage_hook_v1, install_after_codex_metadata_inventory_hook,
    CodexCausalSourceObservationV1,
};

const CURRENT_PARSER_REVISION: &str = "codex-nativepath-core-record-v27-bounded-exact-origin";
const CURRENT_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v14";

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
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:02Z",
        "type": "turn_context",
        "payload": {"cwd": "/tmp/codex-child-independence"}
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

fn exec_result(call_id: &str, marker: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!("{marker}\n0123456789abcdef0123456789abcdef01234567\n")
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
fn child_local_positive_copy_proof_stays_copied_and_absence_stays_unknown() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let parent = "019fb000-0000-7000-8000-000000000021";
    let child = "019fb000-0000-7000-8000-000000000022";
    write_session(
        &sessions,
        parent,
        SessionRelationshipKind::Root,
        None,
        [
            exec_call("copied-call"),
            exec_result("copied-call", "positive-copied-result-marker"),
            message("origin-parent-marker"),
        ],
    );
    write_session(
        &sessions,
        child,
        SessionRelationshipKind::Forked,
        Some(parent),
        [
            exec_call("copied-call"),
            exec_result("copied-call", "positive-copied-result-marker"),
            turn_context(),
            exec_call("local-call"),
            exec_result("local-call", "unknown-local-result-marker"),
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
    let local = records
        .iter()
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("unknown-local-result-marker"))
        })
        .expect("local result record");
    assert_eq!(local.event_origin, EventOrigin::Unknown);
    assert!(!records
        .iter()
        .any(|record| record.event_origin == EventOrigin::UniqueToSession));

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
fn continuation_restart_preserves_exact_result_linkage_and_abstains_without_origin_proof() {
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
    assert!(result.repository_vcs_observations.is_empty());
    assert!(result.repository_abstentions.iter().any(|abstention| {
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
                "codex-nativepath-core-record-v26-child-independent-origin",
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
        assert_eq!(wire["version"], 14);
        assert!(wire.get("certified_lineage_facts").is_none());
        assert!(wire.get("dependency_digest").is_none());
    }
}

#[test]
fn genuine_mid_capture_change_preserves_last_good_generation_atomically() {
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
    install_after_codex_metadata_inventory_hook(move || {
        append_event(&mutate, message("deferredafterfailureuniquetoken"));
    });
    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert!(failed.failed_routes[0].carried_forward);
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), generation);
    assert_eq!(
        source_snapshot(&retained, native_session_id, "lastgooduniquetoken"),
        snapshot
    );
    assert!(retained
        .search_event_candidates("deferredafterfailureuniquetoken", 8)
        .unwrap()
        .is_empty());
    drop(retained);

    let caught_up =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(caught_up.failed_routes.is_empty());
    let current = VerifiedIndex::open(&index_root).unwrap();
    assert_ne!(current.generation_id(), generation);
    assert_eq!(
        current
            .search_event_candidates("deferredafterfailureuniquetoken", 8)
            .unwrap()
            .len(),
        1
    );
}
