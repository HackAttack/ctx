use super::*;

#[test]
fn gemini_result_block_retains_every_native_field_exactly() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("result", "main"),
            json!({"id":"result-only","type":"gemini","toolCalls":[{
                "id":"call-1","name":"run_shell_command","is_error":true,
                "result":{"content":"secret","error":"exact failure","unknown":[1,2,3]},
                "future":{"nested":"retained"}
            }]}),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    let serialized = serde_json::to_string(&rows).unwrap();
    for expected in ["secret", "exact failure", "unknown", "future", "retained"] {
        assert!(serialized.contains(expected));
    }
    let records = project_gemini_test_events(&source, rows).unwrap();
    assert_eq!(
        records[0].content.structured_content.as_ref().unwrap(),
        &json!({
            "id":"call-1","name":"run_shell_command","is_error":true,
            "result":{"content":"secret","error":"exact failure","unknown":[1,2,3]},
            "future":{"nested":"retained"}
        })
    );
}

#[test]
fn gemini_retention_counts_messages_calls_results_and_notices() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("counts", "main")];
    for index in 0..20 {
        values.push(match index % 5 {
            0 => json!({"id":format!("u-{index}"),"type":"user","content":"user"}),
            1 => json!({"id":format!("a-{index}"),"type":"gemini","content":"assistant"}),
            2 => json!({"id":format!("c-{index}"),"type":"gemini","toolCalls":[{
                "id":format!("call-{index}"),"name":"tool","args":{}
            }]}),
            3 => json!({"id":format!("r-{index}"),"type":"gemini","toolCalls":[{
                "id":format!("call-{index}"),"result":"output"
            }]}),
            _ => json!({"id":format!("s-{index}"),"$set":{"summary":"state"}}),
        });
    }
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 20);
    assert_eq!(outcome.metrics.retained_messages, 8);
    assert_eq!(outcome.metrics.retained_tool_calls, 4);
    assert_eq!(outcome.metrics.native_result_records_observed, 4);
    assert_eq!(outcome.metrics.retained_notices, 4);
}

#[test]
fn gemini_fact_count_overflow_withholds_fact_set_without_dropping_event() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let paths = (0..=ctx_history_core::MAX_PROVIDER_DECLARED_FACTS)
        .map(|index| format!("p-{index}"))
        .collect::<Vec<_>>();
    let path = write_transcript(
        &root,
        &[
            header("fact-bound", "main"),
            json!({"id":"many","type":"gemini","toolCalls":[{
                "id":"call","name":"tool","args":{"paths":paths}
            }]}),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 1);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let facts = &records[0].content.activity.as_ref().unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, LiteralFactKind::SessionCwd);
    records[0].validate_contract().unwrap();
}

#[test]
fn gemini_streams_local_scale_without_accumulating_results() {
    const PAIRS: usize = 512;
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    serde_json::to_writer(&mut file, &header("scale", "main")).unwrap();
    file.write_all(b"\n").unwrap();
    for index in 0..PAIRS {
        serde_json::to_writer(
            &mut file,
            &json!({"id":format!("request-{index}"),"type":"gemini","toolCalls":[{
                "id":format!("call-{index}"),"name":"tool","args":{"path":format!("p-{index}")}
            }]}),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
        serde_json::to_writer(
            &mut file,
            &json!({"id":format!("result-{index}"),"type":"gemini","toolCalls":[{
                "id":format!("call-{index}"),"result":{"content":"x".repeat(1024)}
            }]}),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    let source = rediscover(&root, &path);
    reset_gemini_parse_counters();
    let (outcome, retained) = scan_collect(&source, None);
    assert_eq!(retained.len(), PAIRS * 2);
    assert_eq!(outcome.metrics.retained_tool_calls, PAIRS as u64);
    assert_eq!(outcome.metrics.native_result_records_observed, PAIRS as u64);
    assert_eq!(
        gemini_parse_counters(),
        (1 + (PAIRS * 2) as u64, PAIRS as u64)
    );
}
