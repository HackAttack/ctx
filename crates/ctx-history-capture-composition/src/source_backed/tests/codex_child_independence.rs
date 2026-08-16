use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::Command,
    sync::{Arc, Barrier, Mutex},
};

use ctx_history_core::{CertifiedSource, EventOrigin, SessionRelationshipKind, SourceFrontier};
use ctx_history_index::{GenerationWriter, RevalidationTarget, WriterOptions};
use sha2::{Digest, Sha256};

use super::*;
use crate::provider::codex::nativepath::{
    install_after_codex_causal_stage_hook_v1, install_after_codex_metadata_inventory_hook,
    CodexCausalSourceObservationV1,
};
use crate::provider::source_backed::family::jsonl::{
    checkpoint_admitted_revision_for_test, new_prefix_hasher, prefix_digest,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_semantic_preflight_hook,
    set_before_jsonl_terminal_physical_revalidation_hook,
};

const CURRENT_PARSER_REVISION: &str =
    "codex-nativepath-core-record-v32-current-exec-repository-evidence";
const CURRENT_FRONTIER_KIND: &str = "borrowed-jsonl-family-checkpoint-v1";
const LEGACY_CODEX_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v18";

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

fn jsonl_prefix_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = new_prefix_hasher();
    hasher.update(bytes);
    prefix_digest(&hasher)
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

fn mcp_terminal(call_id: &str, server: &str, marker: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": {
                "server": server,
                "tool": "race-tool",
                "arguments": {}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": marker}],
                    "isError": false
                }
            }
        }
    })
}

fn checkpoint_mcp_terminal(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": {
                "server": "checkpoint-envelope",
                "tool": "read",
                "arguments": {}
            },
            "duration": {"secs": 0, "nanos": 1},
            "result": {"Err": "event-body-secret-must-not-reach-checkpoint"}
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

fn codex_test_workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    (temp, sessions, index_root)
}

fn initialized_test_repository(parent: &Path) -> (PathBuf, String) {
    let repository = parent.join("repository");
    initialize_repository(&repository);
    (repository.clone(), repository_head(&repository))
}

fn repository_head(repository: &Path) -> String {
    let oid = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repository)
        .output()
        .unwrap();
    String::from_utf8(oid.stdout).unwrap().trim().to_owned()
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

fn write_session_with_payload_session_id(
    root: &Path,
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
    payload_session_id: &str,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let mut meta = session_meta(native_session_id, relationship, parent_native_session_id);
    meta["payload"]["session_id"] = serde_json::json!(payload_session_id);
    let records = std::iter::once(meta).chain(events);
    fs::write(session_path(root, native_session_id), jsonl_bytes(records)).unwrap();
}

fn replace_session(
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
    replace_session_bytes(root, native_session_id, jsonl_bytes(records));
}

fn replace_session_with_payload_session_id(
    root: &Path,
    native_session_id: &str,
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<&str>,
    payload_session_id: &str,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let mut meta = session_meta(native_session_id, relationship, parent_native_session_id);
    meta["payload"]["session_id"] = serde_json::json!(payload_session_id);
    replace_session_bytes(
        root,
        native_session_id,
        jsonl_bytes(std::iter::once(meta).chain(events)),
    );
}

fn replace_session_bytes(root: &Path, native_session_id: &str, bytes: Vec<u8>) {
    let path = session_path(root, native_session_id);
    let replacement = path.with_extension("replacement");
    fs::write(&replacement, bytes).unwrap();
    fs::remove_file(&path).unwrap();
    fs::rename(replacement, path).unwrap();
}

fn append_event(path: &Path, event: serde_json::Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&jsonl_bytes([event])).unwrap();
    file.sync_all().unwrap();
}

fn destructively_mutate_session(path: &Path, replacement: &Path, mutation: &str) {
    match mutation {
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

fn provider_checkpoint_envelope(
    index: &VerifiedIndex,
    native_session_id: &str,
) -> (usize, usize, usize, serde_json::Value) {
    let certificate = certificate_for(index, native_session_id);
    let frontier = certificate.frontier().unwrap();
    frontier.validate_contract().unwrap();
    let TypedKey::Utf8(family_json) = frontier.checkpoint() else {
        panic!("new family checkpoint was not compact UTF-8");
    };
    let family = serde_json::from_str::<serde_json::Value>(family_json).unwrap();
    let provider = family
        .get("provider_checkpoint")
        .expect("Codex family checkpoint omitted provider state")
        .clone();
    let provider_bytes = provider
        .get("Utf8")
        .and_then(|value| value.as_str())
        .map_or(0, str::len);
    (
        provider_bytes,
        family_json.len(),
        serde_json::to_vec(frontier).unwrap().len(),
        provider,
    )
}

fn certificate_with_provider_checkpoint(
    index: &VerifiedIndex,
    native_session_id: &str,
    provider_checkpoint: TypedKey,
) -> CertifiedSource {
    let current = certificate_for(index, native_session_id);
    let frontier = current.frontier().unwrap();
    let TypedKey::Utf8(family_json) = frontier.checkpoint() else {
        panic!("Codex family checkpoint was not compact UTF-8");
    };
    let mut family = serde_json::from_str::<serde_json::Value>(family_json).unwrap();
    family["provider_checkpoint"] = serde_json::to_value(provider_checkpoint).unwrap();
    let checkpoint = TypedKey::Utf8(serde_json::to_string(&family).unwrap());
    let modified_frontier = SourceFrontier::new(
        frontier.checkpoint_kind(),
        checkpoint,
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    CertifiedSource::certify_with_frontier(
        current.observation().clone(),
        current.observation().clone(),
        current.parser_revision(),
        *current.content_digest(),
        current.counts(),
        Some(modified_frontier),
    )
    .unwrap()
}

fn install_single_source_certificate(
    index_root: &Path,
    native_session_id: &str,
    provider_checkpoint: TypedKey,
) -> String {
    let current = VerifiedIndex::open(index_root).unwrap();
    let routes = current.manifest().source_routes().to_vec();
    let replacement =
        certificate_with_provider_checkpoint(&current, native_session_id, provider_checkpoint);
    let records = records_for(&current, native_session_id);
    assert_eq!(
        routes
            .iter()
            .flat_map(|route| route.sources())
            .filter(|source| source.exact_descriptor_eq(replacement.observation().source()))
            .count(),
        1
    );
    drop(current);

    let mut writer = GenerationWriter::open(index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_source_route_plan(
            routes
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        )
        .unwrap();
    for route in &routes {
        writer
            .begin_source_route_stage(route.route_identity().clone())
            .unwrap();
        for source in route.sources() {
            assert!(source.exact_descriptor_eq(replacement.observation().source()));
            writer.begin_source(source.clone()).unwrap();
            for record in &records {
                writer.add_core_record(record.clone()).unwrap();
            }
            writer.certify_source(replacement.clone()).unwrap();
        }
        writer
            .finish_source_route_stage(route.route_identity())
            .unwrap();
    }
    writer.set_present_source_routes(routes).unwrap();
    writer
        .commit(|target| match target {
            RevalidationTarget::Source(actual) => actual == &replacement,
            RevalidationTarget::Deletion(_) => false,
        })
        .unwrap()
        .generation_id
}

fn retired_semantic_v2_checkpoint(native_session_id: &str) -> TypedKey {
    TypedKey::Utf8(
        serde_json::to_string(&serde_json::json!({
            "version": 2,
            "pending_tool_authorities": [{
                "call_id_sha256": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
                "record_start": 1,
                "record_end": 2,
                "raw_ordinal": 1,
                "continuation_cell_id": null,
                "continuation_conflicted": false,
                "continuation_call_id_sha256": "",
                "continuation_capacity_exceeded": false,
                "correlation_ambiguous": false,
                "invocation_origin": {"kind": "unique_to_session"}
            }],
            "owner": {
                "native_session_id": native_session_id,
                "parent_native_session_id": null,
                "advisory_session_id": native_session_id,
                "root_native_session_id": native_session_id,
                "session_relationship": "root",
                "started_at": "2026-08-09T12:00:00Z",
                "cwd": "/tmp/codex-child-independence",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source_kind": "cli",
                "external_agent_id": null,
                "role_hint": null,
                "model_provider": "openai",
                "git": null
            },
            "local_turn_started": false
        }))
        .unwrap(),
    )
}

fn assert_legacy_provider_checkpoint_is_inert(
    case: &str,
    provider_checkpoint: impl FnOnce(&str) -> TypedKey,
) {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join(format!("sessions-{case}"));
    let index_root = temp.path().join(format!("index-{case}"));
    let cold_root = temp.path().join(format!("cold-{case}"));
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005a";
    let call_id = format!("{case}-pending-call");
    let marker = format!("{case}semanticcheckpointreplacementtoken");
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        SessionRelationshipKind::Root,
        None,
        [turn_context(), exec_call(&call_id)],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let injected_generation = install_single_source_certificate(
        &index_root,
        native_session_id,
        provider_checkpoint(native_session_id),
    );
    let injected = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(injected.generation_id(), injected_generation);
    assert_eq!(
        certificate_for(&injected, native_session_id).parser_revision(),
        CURRENT_PARSER_REVISION
    );
    let injected_certificate_bytes =
        serde_json::to_vec(&certificate_for(&injected, native_session_id)).unwrap();
    drop(injected);

    let unchanged_observed = capture_causal_stage();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, injected_generation);
    let unchanged_counters = causal_by_id(&unchanged_observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(unchanged_counters.scanner_source_opens, 0);
    assert_eq!(unchanged_counters.scanner_sources_started, 0);
    assert_eq!(unchanged_counters.scanner_sources_completed, 0);
    assert_eq!(unchanged_counters.scanner_bytes_read, 0);
    assert_eq!(unchanged_counters.catalog_source_metadata_opens, 0);
    assert_eq!(
        unchanged_counters.catalog_source_metadata_read_upper_bound_bytes,
        0
    );
    assert_eq!(unchanged_counters.catalog_session_meta_parses, 0);
    assert_eq!(unchanged_counters.appended_sources, 0);
    assert_eq!(unchanged_counters.replaced_sources, 0);
    assert_eq!(unchanged_counters.writer_mutated_sources, 0);
    let unchanged_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&unchanged_index, native_session_id)).unwrap(),
        injected_certificate_bytes
    );
    drop(unchanged_index);

    append_event(&path, exec_result(&call_id, &marker));
    let append_observed = capture_causal_stage();
    let appended =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    assert!(appended.logical_source_failures.is_empty());
    let counters = causal_by_id(&append_observed)
        .get(native_session_id)
        .unwrap()
        .counters;
    assert_eq!(counters.appended_sources, 0);
    assert_eq!(counters.replaced_sources, 1);
    assert_eq!(counters.scanner_sources_started, 1);
    assert_eq!(counters.scanner_sources_completed, 1);
    assert_eq!(counters.writer_mutated_sources, 1);

    let rebuilt = VerifiedIndex::open(&index_root).unwrap();
    let rebuilt_snapshot = source_snapshot(&rebuilt, native_session_id, &marker);
    let (_, _, _, rebuilt_checkpoint) = provider_checkpoint_envelope(&rebuilt, native_session_id);
    assert_current_provider_checkpoint(&rebuilt_checkpoint);
    assert_eq!(
        certificate_for(&rebuilt, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&path).unwrap().len()
    );
    drop(rebuilt);

    let cold = refresh_source_backed_generation(&cold_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(
        cold.commit.certified_source_bytes,
        appended.commit.certified_source_bytes
    );
    let cold = VerifiedIndex::open(&cold_root).unwrap();
    assert_eq!(
        source_snapshot(&cold, native_session_id, &marker),
        rebuilt_snapshot
    );
}

fn assert_current_provider_checkpoint(checkpoint: &serde_json::Value) {
    const MAX_PROVIDER_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
    let encoded = checkpoint
        .get("Utf8")
        .and_then(serde_json::Value::as_str)
        .expect("Codex provider checkpoint must be compact UTF-8");
    assert!(encoded.starts_with("zstd-json-v1:"));
    assert!(encoded.len() <= MAX_PROVIDER_CHECKPOINT_BYTES);
}

fn terminal_authority_events(entries: usize) -> Vec<serde_json::Value> {
    (0..entries)
        .map(|index| checkpoint_mcp_terminal(&format!("mcp-checkpoint-{index:03}")))
        .collect()
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
    _parent_native_session_id: Option<&str>,
) {
    let source = sources
        .get(native_session_id)
        .unwrap_or_else(|| panic!("missing causal source {native_session_id}"));
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
    assert!(
        receipt.failed_routes.is_empty(),
        "unexpected failed routes: {:?}",
        receipt.failed_routes
    );
    assert!(receipt.logical_source_failures.is_empty());
    let sources = causal_by_id(&observed);
    for (descendant, parent) in descendants {
        assert_exact_zero_work(&sources, descendant, Some(parent));
    }
    sources
}

fn assert_continuation_restart_exact_commit(
    relationship: SessionRelationshipKind,
    parent: Option<&str>,
    include_turn_context: bool,
) {
    let (temp, sessions, index_root) = codex_test_workspace();
    let (repository, oid) = initialized_test_repository(temp.path());
    let native_session_id = "019fb000-0000-7000-8000-000000000029";
    let path = session_path(&sessions, native_session_id);
    let mut events = Vec::new();
    if include_turn_context {
        events.push(turn_context());
    }
    events.extend([
        exec_call_in(
            "continuation-origin",
            "git commit -m exact && git rev-parse HEAD",
            &repository,
        ),
        running_result("continuation-origin", "cell-exact-7"),
    ]);
    write_session(&sessions, native_session_id, relationship, parent, events);
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    append_event(&path, wait_call("continuation-wait", "cell-exact-7"));
    append_event(
        &path,
        completed_wait_result(
            "continuation-wait",
            format!("[main abc1234] exact\n{oid}\n"),
        ),
    );
    let observed = capture_causal_stage();
    refresh_source_backed_generation_incremental_for_test(&index_root, &registry, writer_options())
        .unwrap();
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
                .is_some_and(|body| body.contains(&oid))
        })
        .unwrap();
    if parent.is_some() {
        assert_eq!(result.parent_session_id, Some(result.root_session_id));
    }
    assert_eq!(result.event_origin, EventOrigin::UniqueToSession);
    assert!(result
        .repository_vcs_observations
        .iter()
        .any(|observation| {
            matches!(
                &observation.kind,
                ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome)
                    if outcome.kind == ctx_history_core::RepositoryOutcomeKind::Commit
                        && outcome.produced_object_ids.iter().any(|object_id| object_id.hex == oid)
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

#[path = "codex_child_independence/lifecycle.rs"]
mod lifecycle;
#[path = "codex_child_independence/repository.rs"]
mod repository;
