use super::*;

fn publish_records(temp: &TempDir, source: &SourceKey, records: Vec<CoreRecord>) -> VerifiedIndex {
    let document_count = u64::try_from(records.len()).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(certificate(source, 1, document_count))
        .unwrap();
    writer.commit(|_| true).unwrap();
    VerifiedIndex::open(temp.path()).unwrap()
}

#[test]
fn script_aware_analysis_indexes_cjk_and_long_technical_identifiers() {
    let temp = tempdir().unwrap();
    let source = source("script-aware.jsonl");
    let cjk = document(&source, 1, "完成数据库迁移并验证索引");
    let long_component = "CtxSourceBackedGenerationIdentifier".repeat(8);
    let technical_identifier =
        format!("crate::provider::{long_component}::<Result<Vec<ProjectionRecord>>>");
    let identifier = document(
        &source,
        2,
        &format!("failed while resolving {technical_identifier}"),
    );
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(cjk.clone()).unwrap();
    writer.add_core_record(identifier.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 2)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        index
            .search_event_candidates("数据库迁移", 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![cjk.event_id]
    );
    assert_eq!(
        index
            .search_event_candidates(&long_component, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![identifier.event_id]
    );
}

#[test]
fn multi_term_search_ranks_full_coverage_before_one_term_partial_matches() {
    let temp = tempdir().unwrap();
    let source = source("coverage-ranking.jsonl");
    let exact = document(&source, 1, "coveragealpha coveragebeta");
    let partial = document(&source, 2, &"coveragealpha ".repeat(64));
    let unrelated = document(&source, 3, "coveragegamma");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(partial.clone()).unwrap();
    writer.add_core_record(unrelated).unwrap();
    writer.add_core_record(exact.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 3)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let candidates = index
        .search_event_candidates("coveragealpha coveragebeta", 10)
        .unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        vec![exact.event_id, partial.event_id]
    );
    assert_eq!(
        index
            .search_event_candidates("coveragealpha coveragebeta", 1)
            .unwrap()[0]
            .event
            .event_id,
        exact.event_id
    );
}

#[test]
fn coverage_tiers_materialize_each_ranked_candidate_once_without_changing_order() {
    let temp = tempdir().unwrap();
    let source = source("coverage-decode-count.jsonl");
    let full = document(&source, 1, "decodealpha decodebeta decodegamma");
    let two_terms = document(
        &source,
        2,
        &format!("{} {}", "decodealpha ".repeat(32), "decodebeta ".repeat(32)),
    );
    let one_term = document(&source, 3, &"decodealpha ".repeat(96));
    let expected = vec![full.event_id, two_terms.event_id, one_term.event_id];
    let expected_encoded_core_bytes = [&full, &two_terms, &one_term]
        .into_iter()
        .map(|record| u64::try_from(record.encode_stored().unwrap().len()).unwrap())
        .sum::<u64>();
    let index = publish_records(&temp, &source, vec![one_term, two_terms, full]);

    ctx_history_index_query::reset_stored_event_record_materializations();
    let observed = observed_candidates(&index, "decodealpha decodebeta decodegamma", 3).unwrap();
    let candidates = observed.candidates;

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.event.event_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        candidates.len(),
        "overlapping lower-coverage tiers must not decode selected Core records again"
    );
    assert_eq!(observed.receipt.query_executions, 3);
    assert_eq!(observed.receipt.collector_hits, 6);
    assert_eq!(observed.receipt.records_decoded, 3);
    assert_eq!(
        observed.receipt.encoded_core_bytes_decoded,
        expected_encoded_core_bytes
    );
}

#[test]
fn empty_and_no_match_queries_distinguish_unattempted_from_exact_zero_work() {
    let (_temp, index) = lexical_query_limit_fixture();

    let empty = observed_candidates(&index, "", 10).unwrap();
    let no_match = observed_candidates(&index, "uniquenonexistentreceiptneedle", 10).unwrap();

    assert_eq!(
        empty.receipt,
        ctx_history_index_query::EventCandidateQueryReceipt::default()
    );
    assert_eq!(no_match.receipt.query_executions, 1);
    assert_eq!(no_match.receipt.collector_hits, 0);
    assert_eq!(no_match.receipt.records_decoded, 0);
    assert_eq!(no_match.receipt.encoded_core_bytes_decoded, 0);
}

#[test]
fn candidate_query_receipt_needs_no_drop() {
    assert!(!std::mem::needs_drop::<
        ctx_history_index_query::EventCandidateQueryReceipt,
    >());
}

#[test]
fn candidate_decode_failure_preserves_completed_low_level_work() {
    let temp = tempdir().unwrap();
    let source = source("partial-failure-receipt.jsonl");
    let first = document(&source, 1, "partialfailurereceiptneedle first");
    let second = document(&source, 2, "partialfailurereceiptneedle second");
    let encoded_sizes = [&first, &second]
        .into_iter()
        .map(|record| {
            (
                record.event_id,
                u64::try_from(record.encode_stored().unwrap().len()).unwrap(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let index = publish_records(&temp, &source, vec![first, second]);
    let successful = observed_candidates(&index, "partialfailurereceiptneedle", 2).unwrap();
    let first_decoded_bytes = encoded_sizes[&successful.candidates[0].event.event_id];

    ctx_history_index_query::fail_lexical_candidate_materialization_after(1);
    let failure = observed_candidates(&index, "partialfailurereceiptneedle", 2).unwrap_err();

    assert!(matches!(
        failure.error,
        IndexError::InvalidStoredDocumentField("test_lexical_candidate_materialization_failure")
    ));
    assert_eq!(failure.receipt.query_executions, 1);
    assert_eq!(failure.receipt.collector_hits, 2);
    assert_eq!(failure.receipt.records_decoded, 1);
    assert_eq!(
        failure.receipt.encoded_core_bytes_decoded,
        first_decoded_bytes
    );
}

#[test]
fn candidate_decode_failure_injection_is_cleared_after_each_query() {
    let temp = tempdir().unwrap();
    let source = source("failure-injection-reset.jsonl");
    let index = publish_records(
        &temp,
        &source,
        vec![
            document(&source, 1, "failureinjectionresetneedle first"),
            document(&source, 2, "failureinjectionresetneedle second"),
        ],
    );

    ctx_history_index_query::fail_lexical_candidate_materialization_after(2);
    observed_candidates(&index, "failureinjectionresetneedle", 1).unwrap();

    observed_candidates(&index, "failureinjectionresetneedle", 2)
        .expect("unused failure injection state must not leak into the next query");
}

fn observed_candidates(
    index: &VerifiedIndex,
    query: &str,
    limit: usize,
) -> ctx_history_index_query::DiagnosedEventCandidateQueryResult {
    index.search_event_candidates_any_with_filters_diagnosed(
        &[query],
        &EventSearchFilters::default(),
        limit,
    )
}

fn lexical_query_limit_fixture() -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let source = source("query-limits.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "bounded lexical query"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn assert_no_lexical_query_was_constructed_or_executed() {
    assert_eq!(ctx_history_index_query::lexical_query_constructions(), 0);
    assert_eq!(ctx_history_index_query::lexical_query_executions(), 0);
}

#[test]
fn lexical_result_limits_reject_oversized_and_usize_max_before_query_work() {
    let (_temp, index) = lexical_query_limit_fixture();
    for requested in [MAX_LEXICAL_QUERY_RESULTS + 1, usize::MAX] {
        ctx_history_index_query::reset_lexical_query_work();
        let error = index
            .search_event_candidates("bounded", requested)
            .unwrap_err();
        assert!(matches!(
            error,
            IndexError::InvalidLexicalResultLimit {
                requested: actual,
                maximum
            }
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();

        ctx_history_index_query::reset_lexical_query_work();
        let error = index
            .list_event_candidates_with_filters(&EventSearchFilters::default(), requested)
            .unwrap_err();
        assert!(matches!(
            error,
            IndexError::InvalidLexicalResultLimit {
                requested: actual,
                maximum
            }
                if actual == requested && maximum == MAX_LEXICAL_QUERY_RESULTS
        ));
        assert_no_lexical_query_was_constructed_or_executed();
    }
}

#[test]
fn oversized_single_query_is_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let oversized = "x".repeat(LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1);
    ctx_history_index_query::reset_lexical_query_work();

    let error = index.search_event_candidates(&oversized, 10).unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryBytesTooLarge {
            actual,
            maximum,
        } if actual == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes + 1
            && maximum == LEXICAL_QUERY_LIMITS.maximum_aggregate_bytes
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn repeated_terms_are_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let alternatives = vec!["bounded"; LEXICAL_QUERY_LIMITS.maximum_alternatives + 1];
    ctx_history_index_query::reset_lexical_query_work();

    let error = index
        .search_event_candidates_any_with_filters(&alternatives, &EventSearchFilters::default(), 10)
        .unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryAlternativesTooMany {
            observed,
            maximum
        }
            if observed == LEXICAL_QUERY_LIMITS.maximum_alternatives + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_alternatives
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}

#[test]
fn analyzed_unique_tokens_are_rejected_before_query_construction() {
    let (_temp, index) = lexical_query_limit_fixture();
    let query = (0..=LEXICAL_QUERY_LIMITS.maximum_unique_tokens)
        .map(|index| format!("uniquetoken{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    ctx_history_index_query::reset_lexical_query_work();

    let error = index.search_event_candidates(&query, 10).unwrap_err();

    assert!(matches!(
        error,
        IndexError::LexicalQueryTokensTooMany {
            observed,
            maximum
        }
            if observed == LEXICAL_QUERY_LIMITS.maximum_unique_tokens + 1
                && maximum == LEXICAL_QUERY_LIMITS.maximum_unique_tokens
    ));
    assert_no_lexical_query_was_constructed_or_executed();
}
