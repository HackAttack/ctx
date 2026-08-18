const PI_V6_PARSER_REVISION: &str = "pi-shared-jsonl-v6-linked-core-activity";
const PI_V8_PARSER_REVISION: &str = "pi-shared-jsonl-v8-optional-activity-admission";

fn pi_parser_revision(data_root: &Path) -> String {
    let index = ctx_history_index::VerifiedIndex::open(data_root.join("search/lexical")).unwrap();
    index
        .manifest()
        .sources
        .iter()
        .find(|certificate| certificate.observation().source().provider() == "pi")
        .expect("published generation must contain the Pi source")
        .parser_revision()
        .to_owned()
}

fn publish_pi_v6_predecessor(data_root: &Path) -> String {
    let index_root = data_root.join("search/lexical");
    let (source, routes, mut certificate, counts) = {
        let index = ctx_history_index::VerifiedIndex::open(&index_root).unwrap();
        let current = index
            .manifest()
            .sources
            .iter()
            .find(|certificate| certificate.observation().source().provider() == "pi")
            .expect("published generation must contain the Pi source");
        assert_eq!(current.parser_revision(), PI_V8_PARSER_REVISION);
        (
            current.observation().source().clone(),
            index.manifest().source_routes().to_vec(),
            serde_json::to_value(current).unwrap(),
            current.counts(),
        )
    };

    let mut records = provider_core_records(data_root, "pi");
    let current_record_count = records.len();
    records
        .retain(|record| !matches!(record.event_type.as_str(), "tool_output" | "command_output"));
    let removed_outputs = u64::try_from(current_record_count - records.len()).unwrap();
    assert_eq!(removed_outputs, 2);
    for record in &mut records {
        record.parser_revision = PI_V6_PARSER_REVISION.to_owned();
    }

    certificate["parser_revision"] = json!(PI_V6_PARSER_REVISION);
    certificate["counts"]["retained_records"] = json!(counts
        .retained_records
        .checked_sub(removed_outputs)
        .unwrap());
    certificate["counts"]["ignored_records"] =
        json!(counts.ignored_records.checked_add(removed_outputs).unwrap());
    certificate["counts"]["indexed_documents"] = json!(records.len());
    let legacy_certificate = serde_json::from_value(certificate).unwrap();

    let mut writer = ctx_history_index::GenerationWriter::open(
        &index_root,
        ctx_history_index::WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.set_present_source_routes(routes).unwrap();
    writer.begin_source(source).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(legacy_certificate).unwrap();
    let legacy_generation = writer.commit(|_| true).unwrap().generation_id;

    let legacy = ctx_history_index::VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(legacy.generation_id(), legacy_generation);
    assert!(legacy.publication_metadata().is_none());
    assert_eq!(pi_parser_revision(data_root), PI_V6_PARSER_REVISION);

    let job = json!({
        "schema_version": 1,
        "owner": "daemon",
        "request_id": "legacy-pi-v6-publication",
        "request_state": "published",
        "status": "completed",
        "operation": "refresh",
        "previous_generation": null,
        "published_generation": legacy_generation.clone(),
        "refresh_scope": { "kind": "all" },
        "daemon_mode": "full",
        "trigger": "periodic",
        "trigger_provenance": "daemon_scheduler",
    });
    assert!(job.get("queued_successors").is_none());
    fs::write(
        data_root.join("daemon/jobs/core-refresh.json"),
        serde_json::to_vec(&job).unwrap(),
    )
    .unwrap();
    legacy_generation
}

#[test]
fn pi_cli_import_search_flow() {
    let temp = tempdir();
    let fixture = temp
        .path()
        .join(".pi/agent/sessions/--workspace--/pi-session.jsonl");
    fs::create_dir_all(fixture.parent().unwrap()).unwrap();
    fs::copy(provider_history_fixture("pi-session.jsonl"), &fixture).unwrap();
    let daemon = start_source_refresh_daemon(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--no-daemon",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    let first_generation = imported["sources"][0]["published_generation"]
        .as_str()
        .expect("Pi provider import must publish a Core generation");

    let search = json_output(ctx(&temp).args([
        "search",
        "provider metadata",
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "pi", "provider metadata");

    drop(daemon);
    let legacy_generation = publish_pi_v6_predecessor(&data_root(&temp));
    assert_ne!(legacy_generation, first_generation);
    assert_eq!(provider_core_counts(&data_root(&temp), "pi"), (1, 4));
    let _daemon = start_source_refresh_daemon(&temp);

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--resume",
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_ne!(
        second["sources"][0]["published_generation"], legacy_generation,
        "an unchanged Pi source must not reuse a v6 projection: {second:#}"
    );
    assert_eq!(pi_parser_revision(&data_root(&temp)), PI_V8_PARSER_REVISION);

    let records = provider_core_records(&data_root(&temp), "pi");
    assert_eq!(provider_core_counts(&data_root(&temp), "pi"), (1, 6));
    assert_eq!(
        records
            .iter()
            .filter(
                |record| record.event_type == "message" && record.role.as_deref() == Some("user")
            )
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event_type == "message"
                && record.role.as_deref() == Some("assistant"))
            .count(),
        1
    );
    let provider_outputs = records
        .iter()
        .filter(|record| matches!(record.event_type.as_str(), "tool_output" | "command_output"))
        .map(|record| (record.event_type.as_str(), record.content.meaningful_text()))
        .collect::<Vec<_>>();
    assert_eq!(
        provider_outputs,
        [
            ("tool_output", "tests passed"),
            ("command_output", "ok token=fixture-secret"),
        ],
        "Core must preserve provider-native output without adjudicating command success"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Pi acceptance must use the Core generation"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}
