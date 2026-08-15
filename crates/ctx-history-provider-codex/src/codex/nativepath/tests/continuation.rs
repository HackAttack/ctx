use super::*;
use std::{cell::RefCell, rc::Rc, sync::Arc};

const LARGE_OCCURRENCES: usize = 16_384;
const REPRESENTATIVE_PREFIX_BYTES: u64 = 180_110_760;
const TINY_APPEND_BYTES: usize = 254;

fn non_display_result(call_id: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "failed"
        }
    }))
}

fn message_with_exact_jsonl_bytes(marker: &str, exact_bytes: usize) -> String {
    let mut event = json!({
        "timestamp": "2026-08-09T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": marker}]
        }
    });
    event["payload"]["continuation_padding"] = json!("");
    let with_field = jsonl(event.clone()).len();
    assert!(with_field <= exact_bytes);
    event["payload"]["continuation_padding"] = json!("x".repeat(exact_bytes - with_field));
    let encoded = jsonl(event);
    assert_eq!(encoded.len(), exact_bytes);
    encoded
}

fn append_ignored_padding_to_exact_len(path: &Path, exact_len: u64) {
    const MAX_PADDING_RECORD_BYTES: usize = 8 * 1024 * 1024;

    let mut remaining = usize::try_from(
        exact_len
            .checked_sub(fs::metadata(path).unwrap().len())
            .expect("padding target exceeds current fixture length"),
    )
    .unwrap();
    let empty = json!({
        "timestamp": "2026-08-09T12:00:05Z",
        "type": "event_msg",
        "payload": {"type": "token_count", "padding": ""}
    });
    let overhead = jsonl(empty).len();
    assert!(remaining >= overhead);
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    while remaining != 0 {
        let mut record_bytes = remaining.min(MAX_PADDING_RECORD_BYTES);
        if remaining > MAX_PADDING_RECORD_BYTES && remaining - record_bytes < overhead {
            record_bytes -= overhead - (remaining - record_bytes);
        }
        assert!(record_bytes >= overhead);
        let record = json!({
            "timestamp": "2026-08-09T12:00:05Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "padding": "x".repeat(record_bytes - overhead)
            }
        });
        let encoded = jsonl(record);
        assert_eq!(encoded.len(), record_bytes);
        file.write_all(encoded.as_bytes()).unwrap();
        remaining -= record_bytes;
    }
    file.sync_all().unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), exact_len);
}

fn row_snapshot(rows: &[ctx_history_core::CoreRecord]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|row| row.encode_stored().unwrap())
        .collect()
}

fn observe_next_source_backed_stage(
    native_session_id: &str,
) -> Rc<
    RefCell<Option<crate::provider::codex::nativepath::source_backed::CodexSourceBackedCountersV0>>,
> {
    let observed = Rc::new(RefCell::new(None));
    let captured = Rc::clone(&observed);
    let native_session_id = native_session_id.to_owned();
    install_after_codex_causal_stage_hook_v1(move |mut sources| {
        assert_eq!(
            sources.len(),
            1,
            "production Codex route observations for {native_session_id}: {sources:?}"
        );
        let source = sources.pop().unwrap();
        assert_eq!(source.provider_session_id, native_session_id);
        let counters = source.counters;
        *captured.borrow_mut() = Some(counters);
    });
    observed
}

#[test]
fn large_prefix_full_route_reconciles_authority_but_projects_only_suffix() {
    let fixture_started = std::time::Instant::now();
    let temp = TempDir::new().unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000009";
    // Production session-tree discovery derives this canonical ID from the
    // filename and therefore performs no transcript-body catalog probe.
    let path = temp
        .path()
        .join(format!("rollout-{native_session_id}.jsonl"));
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(session_meta(native_session_id).as_bytes())
        .unwrap();
    for index in 0..LARGE_OCCURRENCES {
        file.write_all(non_display_result(&format!("completed-prefix-call-{index}")).as_bytes())
            .unwrap();
    }
    file.write_all(message("user", "large-prefix-seed").as_bytes())
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    append_ignored_padding_to_exact_len(&path, REPRESENTATIVE_PREFIX_BYTES);
    let fixture_elapsed = fixture_started.elapsed();

    let coordinator = Arc::new(CodexGenerationNormalizationCoordinatorV0::default());
    let generation = coordinator
        .register_session_tree(vec![temp.path().to_path_buf()])
        .unwrap();
    let participant = generation.participant();
    let adapter = Arc::new(CodexSessionJsonlFamilyAdapterV0::<TestRuntime>::new(
        generation,
    ));
    let driver = crate::provider::source_backed::family::jsonl::provider_jsonl_family_driver::<
        TestRuntime,
    >(adapter, temp.path().to_path_buf());

    let initial_catalog_started = std::time::Instant::now();
    coordinator.prepare(&[participant]).unwrap();
    let initial_catalog_elapsed = initial_catalog_started.elapsed();
    let initial_scan_started = std::time::Instant::now();
    let mut previous = scan_source_backed_generation(&driver, None);
    let initial_scan_elapsed = initial_scan_started.elapsed();
    assert_eq!(previous.sources.len(), 1);
    assert_eq!(
        previous.sources[0].counts().certified_bytes,
        REPRESENTATIVE_PREFIX_BYTES
    );

    let mut append_metrics = Vec::new();
    let mut append_counters = Vec::new();
    for marker in [
        "largeprefixappendoneuniquetoken",
        "largeprefixappendtwouniquetoken",
    ] {
        let suffix = message_with_exact_jsonl_bytes(marker, TINY_APPEND_BYTES);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(suffix.as_bytes())
            .unwrap();
        let prefix_hash = ctx_history_jsonl::track_jsonl_prefix_hash_bytes(path.clone());
        let catalog_started = std::time::Instant::now();
        coordinator.prepare(&[participant]).unwrap();
        let catalog_elapsed = catalog_started.elapsed();
        // The production observer publishes the completed prior stage before
        // this generation installs fresh plans.
        let observed = observe_next_source_backed_stage(native_session_id);
        let scan_started = std::time::Instant::now();
        let previous_record_count = previous.records.len();
        let appended = scan_source_backed_generation(&driver, Some(previous));
        let scan_elapsed = scan_started.elapsed();
        let prior_counters = observed
            .borrow_mut()
            .take()
            .expect("production Codex route did not complete its causal stage");
        if append_metrics.is_empty() {
            assert_eq!(prior_counters.cold_sources, 1);
        } else {
            append_counters.push(prior_counters);
        }
        assert_eq!(prefix_hash.bytes(), 0);
        assert_eq!(appended.records.len(), previous_record_count + 1);
        assert!(appended
            .records
            .last()
            .unwrap()
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| body.contains(marker)));
        append_metrics.push((marker, catalog_elapsed, scan_elapsed, prefix_hash.bytes()));
        previous = appended;
    }

    let final_catalog_started = std::time::Instant::now();
    coordinator.prepare(&[participant]).unwrap();
    let final_catalog_elapsed = final_catalog_started.elapsed();
    let observed = observe_next_source_backed_stage(native_session_id);
    let cold_scan_started = std::time::Instant::now();
    let cold = scan_source_backed_generation(&driver, None);
    let cold_scan_elapsed = cold_scan_started.elapsed();
    append_counters.push(
        observed
            .borrow_mut()
            .take()
            .expect("production Codex route did not publish the final append stage"),
    );
    assert_eq!(
        cold.sources[0].counts().certified_bytes,
        REPRESENTATIVE_PREFIX_BYTES + (2 * TINY_APPEND_BYTES) as u64
    );
    assert_eq!(row_snapshot(&cold.records), row_snapshot(&previous.records));

    eprintln!(
        "Codex production suffix fixture: bytes={REPRESENTATIVE_PREFIX_BYTES} occurrences={LARGE_OCCURRENCES} fixture_ms={} initial_catalog_ms={} initial_cold_scan_ms={} final_catalog_ms={} final_cold_rescan_ms={}",
        fixture_elapsed.as_millis(),
        initial_catalog_elapsed.as_millis(),
        initial_scan_elapsed.as_millis(),
        final_catalog_elapsed.as_millis(),
        cold_scan_elapsed.as_millis(),
    );
    assert_eq!(append_metrics.len(), append_counters.len());
    for (append_index, ((marker, catalog_elapsed, scan_elapsed, prefix_hash_bytes), counters)) in
        append_metrics.into_iter().zip(append_counters).enumerate()
    {
        assert_eq!(counters.catalog_source_metadata_read_upper_bound_bytes, 0);
        assert_eq!(counters.catalog_session_meta_parses, 0);
        // This harness deliberately enters through complete route discovery,
        // not exact watcher/member admission. Reconciliation therefore replays
        // semantic authority from byte zero, while physical projection still
        // resumes at the certified frontier and reads only the new record.
        let reconciled_bytes = REPRESENTATIVE_PREFIX_BYTES
            + u64::try_from(append_index + 1).unwrap() * TINY_APPEND_BYTES as u64;
        assert_eq!(counters.mcp_terminal_authority_bytes_read, reconciled_bytes);
        assert_eq!(
            counters.repository_candidate_authority_bytes_read,
            reconciled_bytes
        );
        assert_eq!(counters.scanner_bytes_read, 254);
        assert_eq!(counters.complete_records_scanned, 1);
        assert_eq!(counters.retained_records_scanned, 1);
        assert_eq!(counters.appended_sources, 1);
        eprintln!(
            "Codex production suffix append: marker={marker} catalog_ms={} suffix_scan_ms={} end_to_end_ms={} catalog_body_read_upper_bound_bytes={} prefix_hash_bytes={} mcp_preflight_bytes={} repository_preflight_bytes={} projection_bytes={} complete_records={} retained_records={} ignored_records={} structural_json_parses={} typed_json_parses={}",
            catalog_elapsed.as_millis(),
            scan_elapsed.as_millis(),
            catalog_elapsed.saturating_add(scan_elapsed).as_millis(),
            counters.catalog_source_metadata_read_upper_bound_bytes,
            prefix_hash_bytes,
            counters.mcp_terminal_authority_bytes_read,
            counters.repository_candidate_authority_bytes_read,
            counters.scanner_bytes_read,
            counters.complete_records_scanned,
            counters.retained_records_scanned,
            counters.ignored_records_scanned,
            counters.structural_json_parses,
            counters.typed_json_parses,
        );
    }
}
