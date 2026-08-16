use super::*;

#[test]
fn gemini_pages_at_physical_record_bound() {
    const EVENTS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS * 2 + 7;
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("page-records", "main")];
    values.extend((0..EVENTS).map(|index| {
        json!({"id":format!("event-{index}"),"type":"gemini","content":format!("message {index}")})
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut physical = 0;
    let mut retained = 0;
    let mut pages = 0;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.retained_event_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        physical += page.physical_records;
        retained += page.events.len();
        pages += 1;
    }
    assert_eq!((physical, retained, pages), (EVENTS + 1, EVENTS, 3));
}

#[test]
fn gemini_pages_at_retained_byte_bound() {
    const CONTENT_BYTES: usize = 2_100_000;
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("page-bytes", "main"),
            json!({"id":"large-1","type":"gemini","content":"a".repeat(CONTENT_BYTES)}),
            json!({"id":"large-2","type":"gemini","content":"b".repeat(CONTENT_BYTES)}),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut event_pages = 0;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        event_pages += usize::from(!page.events.is_empty());
    }
    assert_eq!(event_pages, 2);
}

#[test]
fn gemini_single_exact_result_may_roll_past_page_target() {
    let exact = format!(
        "{}gemini-full-result-tail",
        "x".repeat(MAX_GEMINI_NATIVE_PAGE_BYTES + 64 * 1024)
    );
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("large-exact-result", "main"),
            json!({"id":"large-result","type":"gemini","toolCalls":[{
                "id":"large-call","result":exact
            }]}),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut rows = Vec::new();
    let mut rolled = false;
    while let Some(page) = reader.next_page().unwrap() {
        rolled |= page.physical_records == 1
            && page.conservative_serialized_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES
            && page.conservative_serialized_bytes <= MAX_GEMINI_SINGLE_RECORD_PAGE_BYTES;
        rows.extend(page.events);
    }
    assert!(rolled);
    assert!(matches!(
        &rows[0].body,
        GeminiEventBody::ToolResult { result: Some(Value::String(value)), .. }
            if value.ends_with("gemini-full-result-tail")
    ));
}

#[test]
fn gemini_result_with_thirty_seven_megabyte_naive_projection_uses_one_exact_body() {
    // A roughly 9 MiB native string formerly appeared in the normalized body,
    // outer structured content, result text, and result JSON: about 37 MiB.
    const RESULT_BYTES: usize = 9 * 1024 * 1024;
    let exact = format!("{}GEMINI_COMPLETE_CANARY", "x".repeat(RESULT_BYTES));
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("large-core-result", "main"),
            json!({"id":"large-result","type":"gemini","toolCalls":[{
                "id":"large-call","name":"read_file","result":exact
            }]}),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 1);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let record = &records[0];
    let normalized = record.content.normalized_body.as_deref().unwrap();
    assert!(normalized.len() >= RESULT_BYTES);
    assert!(normalized.ends_with("GEMINI_COMPLETE_CANARY"));
    assert!(record.content.structured_content.is_none());
    let result = record
        .content
        .activity
        .as_ref()
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert!(matches!(
        &result.text,
        ActivityTextCapture::Omitted {
            reason: value,
            observed_bytes: Some(bytes),
        } if value == "size_limit" && *bytes >= RESULT_BYTES as u64
    ));
    assert!(matches!(
        result.structured_content,
        ActivityJsonCapture::Omitted {
            reason: ref value,
            observed_encoded_bytes: Some(bytes),
        } if value == "size_limit" && bytes >= RESULT_BYTES as u64
    ));
    assert!(serde_json::to_vec(record).unwrap().len() < ctx_history_core::MAX_CORE_CONTENT_BYTES);
    record.validate_contract().unwrap();
}

#[test]
fn gemini_safe_pages_chain_frontiers_and_identities() {
    const CONTENT_BYTES: usize = 2_100_000;
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut records = vec![header("frontiers", "main")];
    records.extend((0..3).map(|index| {
        json!({"id":format!("large-{index}"),"type":"gemini","content":"x".repeat(CONTENT_BYTES)})
    }));
    let path = write_transcript(&root, &records);
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut previous = None;
    let mut ids = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        if let Some(previous) = previous.as_ref() {
            assert_eq!(&page.expected_frontier, previous);
        }
        assert_ne!(page.identity.as_bytes(), &[0; 32]);
        ids.push(page.identity);
        previous = Some(page.next_safe_frontier);
    }
    assert_eq!(ids.len(), 3);
    assert_eq!(
        previous.unwrap().complete_prefix_end,
        reader.outcome().unwrap().checkpoint.complete_prefix_end
    );
}
