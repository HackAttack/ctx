use ctx_history_core::{
    McpExchangeContent, McpInvocationContent, McpJsonCapture, McpTerminalResponseContent,
    McpTerminalStatus, McpTextCapture, McpToolCallAttribution,
};

use super::*;
use crate::commands::source_index::{mcp_show_event, mcp_show_event_with_compact};

const ARGUMENT_SEARCH_CANARY: &str = "zzargumentcanary8h63";
const CALL_ID_SEARCH_CANARY: &str = "zzcallidcanary7g52";
const RESPONSE_SEARCH_CANARY: &str = "zzresponsecanary9j74";
const COPIED_SEARCH_CANARY: &str = "zzcopiedlineagecanary6k41";

fn complete_exchange(payload: Value) -> McpExchangeContent {
    McpExchangeContent {
        provider_call_id: "native-call-呼び出し-🦀".to_owned(),
        invocation: Some(McpInvocationContent {
            server: "mcp-サーバー".to_owned(),
            tool: "検索-tool".to_owned(),
            arguments: McpJsonCapture::Present {
                value: json!({
                    "snake_key": ["雪", null, {"camelKey": true}],
                    "nested": {"deep_null": null},
                }),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present { value: payload },
        }),
    }
}

fn lineage_models(root: &Path, selected: &CoreEventRecord) -> (Value, Value, Value) {
    let index = open_index(root).unwrap();
    let cli = super::super::copied_lineage::copied_lineage_value(
        &index,
        selected.event_id.as_uuid(),
        ctx_history_index::SHOW_COPIED_EVENT_LINEAGE_POLICY,
    )
    .unwrap();
    let (mcp, compact) = mcp_show_event_with_compact(
        root,
        &selected.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    (cli, mcp, compact)
}

#[test]
fn full_show_surfaces_mcp_exchange_losslessly_and_accounts_for_its_output_bytes() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1);
    let payload = json!({
        "result_key": ["完了", null, {"mixedCase": [false, 3]}],
        "large": "x".repeat(8 * 1024),
    });
    let exchange = complete_exchange(payload.clone());
    let exact_exchange = serde_json::to_value(&exchange).unwrap();
    let mut core_event = fixture_core_event(&event, "normalized response body");
    core_event.core_record.mcp_tool_call = Some(McpToolCallAttribution {
        server: "mcp-サーバー".to_owned(),
        tool: "検索-tool".to_owned(),
    });
    core_event.core_record.content.mcp_exchange = Some(exchange);
    core_event.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&core_event), 94);

    let rendered = render_event_value(&core_event);
    assert_eq!(rendered["mcp_exchange"], exact_exchange);
    assert_eq!(
        rendered["mcp_exchange"]["response"]["payload"]["value"],
        payload
    );
    assert!(rendered["mcp_exchange"]["response"]["payload"]["value"]["result_key"][1].is_null());
    assert_eq!(rendered["text"], "normalized response body");
    assert_eq!(rendered["mcp_tool_call"]["server"], "mcp-サーバー");

    let shown = mcp_fixture_show_event(temp.path(), &core_event);
    assert_eq!(shown["event"]["mcp_exchange"], exact_exchange);
    let session = SessionRecord::from(&core_event.event);
    let shown_session = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        10,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(shown_session["events"][0]["mcp_exchange"], exact_exchange);

    let content = &core_event.core_record.content;
    let expected_preflight_bytes = 2_usize
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.normalized_body).unwrap(),
        )
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.structured_content).unwrap(),
        )
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.mcp_exchange).unwrap(),
        );
    let error = render_event_values(&[&core_event], expected_preflight_bytes - 1).unwrap_err();
    let typed = error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("MCP exchange should participate in the content preflight");
    assert_eq!(typed.actual_bytes, expected_preflight_bytes);
    assert_eq!(typed.maximum_bytes, expected_preflight_bytes - 1);

    let bounded_error = mcp_show_event(
        temp.path(),
        &core_event.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        1024,
    )
    .unwrap_err();
    let bounded = bounded_error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("MCP show-event should reject an oversized exchange response");
    assert_eq!(bounded.maximum_bytes, 1024);
    assert!(bounded.actual_bytes > bounded.maximum_bytes);

    let absent = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 95, 1),
        "no exchange",
    );
    assert!(render_event_value(&absent).get("mcp_exchange").is_none());
}

#[test]
fn search_snippets_use_mcp_invocation_arguments_but_exclude_response_and_call_id() {
    let temp = tempdir().unwrap();
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 96, 1);
    let exchange = McpExchangeContent {
        provider_call_id: CALL_ID_SEARCH_CANARY.to_owned(),
        invocation: Some(McpInvocationContent {
            server: "mcp-検索サーバー".to_owned(),
            tool: "nested_lookup_tool".to_owned(),
            arguments: McpJsonCapture::Present {
                value: json!({
                    "outer": {
                        "雪": ["東京", {"argument_only": ARGUMENT_SEARCH_CANARY}],
                    },
                }),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present {
                value: json!({"response_only": RESPONSE_SEARCH_CANARY}),
            },
        }),
    };
    let exact_exchange = serde_json::to_value(&exchange).unwrap();
    let mut stored = fixture_core_event(&event, "ordinary stored response body");
    stored.core_record.mcp_tool_call = Some(McpToolCallAttribution {
        server: "mcp-検索サーバー".to_owned(),
        tool: "nested_lookup_tool".to_owned(),
    });
    stored.core_record.content.mcp_exchange = Some(exchange);
    stored.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 96);

    let mut argument_request = request(RefreshArg::Off);
    argument_request.query = ARGUMENT_SEARCH_CANARY.to_owned();
    argument_request.events = true;
    argument_request.limit = 1;
    let (value, collection, _) = search_existing_generation(
        &argument_request,
        open_index(temp.path()).unwrap(),
        temp.path(),
        argument_request.semantic_weight,
        "existing_generation",
        1,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 1);
    assert_eq!(
        value["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    let snippet = value["results"][0]["snippet"].as_str().unwrap();
    assert!(snippet.contains(ARGUMENT_SEARCH_CANARY));
    assert!(snippet.contains("東京"));
    assert!(!snippet.contains(CALL_ID_SEARCH_CANARY));
    assert!(!snippet.contains(RESPONSE_SEARCH_CANARY));

    let (mcp_value, _) = mcp_search(argument_request, temp.path()).unwrap();
    assert_eq!(mcp_value["results"][0]["snippet"], snippet);
    assert_eq!(
        mcp_value["results"][0]["session_relationship"],
        value["results"][0]["session_relationship"]
    );
    assert_eq!(
        mcp_value["results"][0]["event_origin"],
        value["results"][0]["event_origin"]
    );

    let shown = mcp_fixture_show_event(temp.path(), &stored);
    assert_eq!(shown["event"]["text"], "ordinary stored response body");
    assert_eq!(shown["event"]["mcp_exchange"], exact_exchange);

    for excluded in [CALL_ID_SEARCH_CANARY, RESPONSE_SEARCH_CANARY] {
        let mut excluded_request = request(RefreshArg::Off);
        excluded_request.query = excluded.to_owned();
        excluded_request.events = true;
        let (value, collection, _) = search_existing_generation(
            &excluded_request,
            open_index(temp.path()).unwrap(),
            temp.path(),
            excluded_request.semantic_weight,
            "existing_generation",
            1,
        )
        .unwrap();
        assert!(collection.result_window.hits.is_empty());
        assert!(value["results"].as_array().unwrap().is_empty());
    }
}

#[test]
fn copied_text_stays_unranked_while_search_and_show_return_full_id_lineage() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let ancestor = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 97, 1);
    let copied = fixture_copied_event(98, &ancestor, &ancestor);
    let ancestor = fixture_core_event(&ancestor, "ancestor body");
    let copied = fixture_core_event(&copied, COPIED_SEARCH_CANARY);
    append_fixture_session(temp.path(), std::slice::from_ref(&ancestor), 97);
    append_fixture_session(temp.path(), std::slice::from_ref(&copied), 98);

    let mut search = request(RefreshArg::Off);
    search.query = COPIED_SEARCH_CANARY.to_owned();
    search.events = true;
    search.limit = 10;
    let (searched, _) = mcp_search(search, temp.path()).unwrap();
    assert!(searched["results"].as_array().unwrap().is_empty());

    let mut canonical_search = request(RefreshArg::Off);
    canonical_search.query = "ancestor body".to_owned();
    canonical_search.events = true;
    canonical_search.limit = 10;
    let config = config::AppConfig::load(temp.path()).unwrap();
    let (canonical_results, _, compact_results) =
        mcp_search_with_compact(canonical_search, temp.path(), &config).unwrap();
    let lineage = &canonical_results["results"][0]["copied_lineage"];
    let lineage = lineage.as_object().unwrap();
    assert_eq!(lineage["schema_version"], 2);
    assert_eq!(lineage["resolution"]["state"], "resolved");
    assert_eq!(lineage["selected_depth"], 0);
    assert_eq!(lineage["observed_count"], 1);
    assert_eq!(lineage["returned"], 1);
    assert_eq!(lineage["truncated"], false);
    assert!(lineage["relationship_counts"].is_object());
    let occurrence = &lineage["occurrences"][0];
    let copied_event_id = copied.event_id.as_uuid().to_string();
    assert_eq!(occurrence["ctx_event_id"], copied_event_id);
    assert_eq!(
        occurrence["ctx_session_id"],
        copied.session_id.as_uuid().to_string()
    );
    assert_eq!(
        occurrence["claimed_root_ctx_session_id"],
        ancestor.session_id.as_uuid().to_string()
    );
    assert!(lineage.get("more_available").is_none());
    let compact_result = &compact_results["results"][0];
    let compact_occurrence = &compact_result["copied_lineage"]["occurrences"][0];
    for reference in [
        compact_result["ctx_event_id"].as_str().unwrap(),
        compact_result["ctx_session_id"].as_str().unwrap(),
        compact_occurrence["ctx_event_id"].as_str().unwrap(),
        compact_occurrence["ctx_session_id"].as_str().unwrap(),
        compact_occurrence["copied_from_ctx_event_id"]
            .as_str()
            .unwrap(),
        compact_occurrence["claimed_root_ctx_session_id"]
            .as_str()
            .unwrap(),
    ] {
        assert!((8..=32).contains(&reference.len()), "{reference}");
        assert!(!reference.contains('-'), "{reference}");
    }
    assert_ne!(
        compact_result["ctx_event_id"],
        canonical_results["results"][0]["ctx_event_id"]
    );
    assert!(compact_result["suggested_next_commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .all(|command| !command.contains(&ancestor.event_id.as_uuid().to_string())));

    let shown = mcp_fixture_show_event(temp.path(), &copied);
    let shown_event = &shown["event"];
    assert_eq!(shown_event["session_relationship"], "forked");
    assert_eq!(
        shown_event["event_origin"],
        event_origin_json(&copied.event_origin)
    );
    assert_eq!(shown_event["text"], COPIED_SEARCH_CANARY);
    assert_eq!(shown["copied_lineage"]["observed_count"], 1);
    assert_eq!(
        shown["copied_lineage"]["occurrences"][0]["ctx_event_id"],
        copied_event_id
    );

    let queried =
        crate::mcp::query_events_for_test(&json!({"content": "full", "limit": 100}), temp.path())
            .unwrap();
    let queried_event = queried["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["ctx_event_id"] == json!(copied.event_id.as_uuid()))
        .expect("copied event remains addressable through query_events");
    assert_eq!(queried_event["session_relationship"], "forked");
    assert_eq!(queried_event["event_origin"], shown_event["event_origin"]);
    assert_eq!(queried_event["text"], COPIED_SEARCH_CANARY);
}

#[test]
fn cli_and_mcp_lineage_contracts_report_unresolved_and_cyclic() {
    let unresolved_root = tempdir().unwrap();
    write_test_generation(unresolved_root.path());
    let absent = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 99, 1);
    let selected = fixture_copied_event(100, &absent, &absent);
    let selected = fixture_core_event(&selected, "selected copied event with absent target");
    append_fixture_session(unresolved_root.path(), std::slice::from_ref(&selected), 100);

    let (cli_unresolved, mcp_unresolved, compact_unresolved) =
        lineage_models(unresolved_root.path(), &selected);
    let absent_event_id = absent.event_id.as_uuid().to_string();
    assert_eq!(cli_unresolved["resolution"]["state"], "unresolved");
    assert_eq!(cli_unresolved["selected_depth"], 1);
    assert_eq!(
        cli_unresolved["resolution"]["ctx_event_id"],
        absent_event_id
    );
    assert_eq!(mcp_unresolved["copied_lineage"], cli_unresolved);
    assert_eq!(
        compact_unresolved["copied_lineage"]["resolution"]["ctx_event_id"],
        absent_event_id
    );
    assert_eq!(
        compact_unresolved["copied_lineage"]["occurrences"][0]["copied_from_ctx_event_id"],
        absent_event_id
    );
    assert!(crate::mcp::render_tool_text_for_test(&compact_unresolved)
        .contains("resolution: unresolved, selected_depth=1"));

    let cyclic_root = tempdir().unwrap();
    write_test_generation(cyclic_root.path());
    let claimed_root = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 101, 1);
    let second = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 103, 1);
    let first = fixture_copied_event(102, &second, &claimed_root);
    let second = fixture_copied_event(103, &first, &claimed_root);
    let first = fixture_core_event(&first, "first cyclic copied event");
    let second = fixture_core_event(&second, "second cyclic copied event");
    append_fixture_session(cyclic_root.path(), std::slice::from_ref(&first), 102);
    append_fixture_session(cyclic_root.path(), std::slice::from_ref(&second), 103);

    let (cli_cyclic, mcp_cyclic, compact_cyclic) = lineage_models(cyclic_root.path(), &first);
    assert_eq!(cli_cyclic["resolution"]["state"], "cyclic");
    assert_eq!(cli_cyclic["selected_depth"], 2);
    assert_eq!(mcp_cyclic["copied_lineage"], cli_cyclic);
    assert_eq!(
        compact_cyclic["copied_lineage"]["occurrences"][0]["claimed_root_ctx_session_id"],
        claimed_root.session_id.as_uuid().to_string()
    );
    assert!(crate::mcp::render_tool_text_for_test(&compact_cyclic)
        .contains("resolution: cyclic, selected_depth=2"));
}
