use std::{path::Path, time::Duration};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_index_query::{
    CoreEventPageBudget, CoreEventRangeFilters, CoreEventRangeSelection, SearchContentScope,
    VerifiedIndex,
};
use serde_json::json;
use tempfile::tempdir;

use crate::{
    copied_lineage_read_model, decode_session_event_cursor, encode_session_event_cursor,
    event_query_event_read_model, event_query_receipt, event_query_wire_request,
    event_window_with_lineage_read_model, normalize_uuid_prefix,
    paginated_session_transcript_read_model, plan_search, render_event_read_model,
    render_search_json, retain_structured_session_page, ActiveSessionExclusion,
    CompactPresentationProjection, EventContentProjection, EventWindowBudget, HistorySemanticBatch,
    HistorySemanticError, HistorySemanticPort, HistorySemanticQuery, ListEventsRequest,
    LocateRequest, LocateResult, PinnedHistoryQuery, SearchBackend, SearchJsonInput, SearchPolicy,
    SearchRenderMetrics, SearchRequest, SearchResultCommands, SemanticReason, SessionEventMode,
    ShowEventRequest, ShowSessionPageRequest, StructuredOutputFormat, StructuredTranscriptMode,
    UuidPrefixError,
};

struct UnusedSemanticPort;

struct UnusedSemanticQuery;

impl HistorySemanticPort for UnusedSemanticPort {
    type Query<'a> = UnusedSemanticQuery;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        panic!("lexical application query must not open the semantic port")
    }
}

impl HistorySemanticQuery for UnusedSemanticQuery {
    fn candidates(
        &mut self,
        _query: &str,
        _filters: &ctx_history_index_query::EventSearchFilters,
        _candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        panic!("lexical application query must not request semantic candidates")
    }
}

fn source() -> SourceKey {
    SourceKey::derive(
        "custom",
        "application_query_test",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("application-query.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64, role: &str, body: &str) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("pinned-session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "application-query-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("pinned-session".to_owned());
    record.occurred_at_unix_ms = Some(1_000 + sequence as i64);
    record.role = Some(role.to_owned());
    record
}

fn certificate(source: &SourceKey, documents: usize) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "application-query-test-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: documents as u64,
            retained_records: documents as u64,
            indexed_documents: documents as u64,
            certified_bytes: documents as u64 * 10,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(root: &Path) -> (VerifiedIndex, Vec<CoreRecord>) {
    let source = source();
    let records = vec![
        record(&source, 1, "user", "needle first"),
        record(&source, 2, "assistant", "needle reply"),
        record(&source, 3, "user", "needle followup"),
    ];
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in &records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(&source, records.len()))
        .unwrap();
    writer.commit(|_| true).unwrap();
    (VerifiedIndex::open_pinned(root).unwrap(), records)
}

fn lexical_request() -> SearchRequest {
    SearchRequest {
        query: "needle".to_owned(),
        terms: Vec::new(),
        limit: 10,
        provider: None,
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        workspace: None,
        since: None,
        primary_only: false,
        include_subagents: true,
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        events: true,
        include_current_session: false,
        backend: Some(SearchBackend::Lexical),
        semantic_weight: 0.35,
    }
}

#[test]
fn one_pin_owns_search_locate_show_and_list_application_workflows() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);

    let search = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            None,
            &UnusedSemanticPort,
        )
        .unwrap();
    assert_eq!(search.collection.result_window.hits.len(), 3);
    assert_eq!(search.presentations.len(), 3);
    assert_eq!(search.copied_lineages.len(), 3);

    let excluded = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            Some(&ActiveSessionExclusion {
                provider: "custom".to_owned(),
                provider_session_id: "pinned-session".to_owned(),
            }),
            &UnusedSemanticPort,
        )
        .unwrap();
    assert!(excluded.collection.result_window.hits.is_empty());

    let LocateResult::Event(located) = query
        .locate(&LocateRequest::Event {
            selector: records[1].event_id.to_string(),
        })
        .unwrap()
    else {
        panic!("event locate returned a session")
    };
    assert_eq!(located.event_id, records[1].event_id);

    let shown = query
        .show_event(&ShowEventRequest {
            selector: records[1].event_id.to_string(),
            before: 1,
            after: 1,
            window: None,
            budget: EventWindowBudget::default(),
        })
        .unwrap();
    assert_eq!(shown.selected.event_id, records[1].event_id);
    assert_eq!(shown.events.len(), 3);

    let session_page = query
        .show_session_page(&ShowSessionPageRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            mode: SessionEventMode::Full,
            cursor: None,
            limit: 2,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
        })
        .unwrap();
    assert_eq!(session_page.events.len(), 2);
    assert!(session_page.has_more);
    assert!(session_page.next_cursor.is_some());

    let listed = query
        .list_events(&ListEventsRequest {
            since: None,
            until: None,
            filters: CoreEventRangeFilters::default(),
            cursor: None,
            limit: 10,
            page_items: 10,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        })
        .unwrap();
    assert_eq!(listed.page.items.len(), 3);
}

#[test]
fn structured_read_models_are_composed_from_pinned_query_results() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);

    let search = query
        .search(
            plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            None,
            &UnusedSemanticPort,
        )
        .unwrap();
    let copied_lineages = search
        .copied_lineages
        .iter()
        .map(copied_lineage_read_model)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let commands = search
        .collection
        .result_window
        .hits
        .iter()
        .map(|hit| SearchResultCommands {
            suggested_next_commands: vec![format!("adapter command {}", hit.event.event_id)],
        })
        .collect::<Vec<_>>();
    let search_value = render_search_json(SearchJsonInput {
        request: &search.request,
        index: &index,
        collection: &search.collection,
        filters: &search.filters,
        presentations: &search.presentations,
        copied_lineages: &copied_lineages,
        commands: &commands,
        freshness_mode: "checkpoint",
        generated_at: "2026-08-11T12:00:00.000Z",
        semantic_fallback_code: None,
        semantic_fallback_detail: None,
        metrics: SearchRenderMetrics {
            refresh_status: "unchanged",
            refresh_source_count: 1,
            query_duration: Duration::from_millis(125),
        },
    })
    .unwrap();
    assert_eq!(search_value["schema_version"], 1);
    assert_eq!(search_value["payload_type"], "search_results");
    assert_eq!(search_value["generated_at"], "2026-08-11T12:00:00.000Z");
    assert_eq!(search_value["freshness"]["mode"], "checkpoint");
    assert_eq!(search_value["phase_attribution"]["query_seconds"], 0.125);
    assert_eq!(search_value["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        search_value["results"][0]["suggested_next_commands"],
        json!([format!(
            "adapter command {}",
            search.collection.result_window.hits[0].event.event_id
        )])
    );

    let shown = query
        .show_event(&ShowEventRequest {
            selector: records[1].event_id.to_string(),
            before: 1,
            after: 1,
            window: None,
            budget: EventWindowBudget::default(),
        })
        .unwrap();
    let event_window = event_window_with_lineage_read_model(
        &shown.selected,
        &shown.events,
        &shown.copied_lineage,
        StructuredOutputFormat::Json,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
    )
    .unwrap();
    assert_eq!(event_window["target"], "event");
    assert_eq!(event_window["event"]["text"], "needle reply");
    assert_eq!(event_window["event"]["event_origin"]["kind"], "unknown");

    let compact = CompactPresentationProjection::new(&index, None)
        .project(&event_window)
        .unwrap();
    assert_ne!(
        compact["ctx_event_id"],
        shown.selected.event_id.as_uuid().to_string()
    );
    assert_eq!(
        event_window["ctx_event_id"],
        shown.selected.event_id.as_uuid().to_string()
    );

    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters::default()).unwrap();
    let wire = event_query_wire_request(&selection, EventContentProjection::Text, 250);
    assert_eq!(wire.domain, json!({ "kind": "all" }));
    assert_eq!(wire.filters, json!({}));
    assert_eq!(wire.direction, "ascending");
    assert_eq!(wire.page_items(), 100);
    let receipt = serde_json::to_value(event_query_receipt(
        &index,
        &wire,
        index.generation_id(),
        None,
        false,
        true,
    ))
    .unwrap();
    assert_eq!(receipt["generation_id"], index.generation_id());
    assert_eq!(receipt["freshness"]["mode"], "pinned");
    assert_eq!(receipt["freshness"]["read_only"], true);
    assert_eq!(receipt["frontier"]["status"], "unavailable");

    let listed = query
        .list_events(&ListEventsRequest {
            since: None,
            until: None,
            filters: CoreEventRangeFilters::default(),
            cursor: None,
            limit: 1,
            page_items: 1,
            byte_budget: ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
            strict_budget: None,
        })
        .unwrap();
    let listed_value =
        render_event_read_model(&listed.page.items[0], EventContentProjection::Text).unwrap();
    assert_eq!(listed_value["content_projection"], "text");
    assert_eq!(listed_value["text"], "needle first");
    assert!(listed_value
        .get("structured_content")
        .is_some_and(|value| value.is_null()));
    let record = event_query_event_read_model(index.generation_id(), 0, listed_value);
    assert_eq!(record["record_type"], "event_range_event");
    assert_eq!(record["ordinal"], 0);
    assert_eq!(record["event"]["text"], "needle first");
}

#[test]
fn structured_cursor_and_compact_reference_compatibility_are_neutral() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let query = PinnedHistoryQuery::new(&index, None);
    let page = query
        .show_session_page(&ShowSessionPageRequest {
            selector: Some(records[0].session_id.to_string()),
            provider_session_id: None,
            provider: None,
            mode: SessionEventMode::Full,
            cursor: None,
            limit: 1,
            page_items: 1,
            page_budget: CoreEventPageBudget::new(
                ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                ctx_history_core::MAX_CORE_CONTENT_BYTES,
            ),
        })
        .unwrap();
    let cursor = page.next_cursor.clone().unwrap();
    let rendered = retain_structured_session_page(
        page.events,
        page.has_more,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
    )
    .unwrap();
    let transcript = paginated_session_transcript_read_model(
        &page.session,
        StructuredTranscriptMode::Full,
        StructuredOutputFormat::Json,
        rendered.events,
        1,
        rendered.has_more,
        rendered.next_cursor.as_ref(),
    )
    .unwrap();
    assert_eq!(transcript["pagination"]["limit"], 1);
    assert_eq!(transcript["pagination"]["returned"], 1);
    assert_eq!(transcript["pagination"]["has_more"], true);
    let encoded = encode_session_event_cursor(&cursor).unwrap();
    assert_eq!(decode_session_event_cursor(&encoded).unwrap(), cursor);

    assert_eq!(normalize_uuid_prefix(" ABCDEF12 ").unwrap(), "abcdef12");
    assert_eq!(
        normalize_uuid_prefix("abcdef").unwrap_err(),
        UuidPrefixError::TooShort
    );
    assert_eq!(
        normalize_uuid_prefix("abcdef1-").unwrap_err(),
        UuidPrefixError::InvalidHex
    );
}
