use ctx_history_core::CaptureProvider;
use rusqlite::{params, params_from_iter};
use uuid::Uuid;

use super::super::{event_query::lexical_event_search_query, projections::fts_match_clauses};
use super::{local_preview_event, record_with_id, tempdir, with_occurred_at};
use crate::Store;

#[test]
fn lexical_event_search_groups_projection_rows_by_event_and_preserves_hit_metadata() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000090001").unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, cwd, raw_source_path, source_format,
             started_at_ms, fidelity, metadata_json)
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/workspace/ctx',
                    '/history/session.jsonl', 'codex_session_jsonl', 1, 'full', ?2)
            "#,
            params![
                source_id.to_string(),
                serde_json::json!({
                    "source_metadata": {
                        "ctx_history_plugin": {
                            "plugin_name": "test-plugin",
                            "plugin_source_id": "source-7",
                            "history_source": "test-plugin/history"
                        },
                        "ctx_history_jsonl_v1": {
                            "provider_key": "provider-7",
                            "source_id": "source-7",
                            "source_format": "test-jsonl"
                        }
                    }
                })
                .to_string(),
            ],
        )
        .unwrap();

    let record = record_with_id(
        "018f45d0-0000-7000-8000-000000090002",
        "Projection metadata title",
        "Projection metadata body",
    );
    store.insert_record(&record).unwrap();
    let mut event = local_preview_event(1, "projection duplicate oracle");
    event.history_record_id = Some(record.id);
    event.capture_source_id = Some(source_id);
    event.payload = serde_json::json!({
        "text": "projection duplicate oracle",
        "cursor": "cursor-7"
    });
    store.upsert_event(&event).unwrap();

    store
        .conn
        .execute(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, NULL, NULL, 'assistant',
                    'projection duplicate oracle stale copy', 'message')
            "#,
            [event.id.to_string()],
        )
        .unwrap();

    let hits = store
        .search_event_hits("projection duplicate oracle", 10)
        .unwrap();
    assert_eq!(hits.len(), 1, "one event must produce one ranked hit");
    let hit = &hits[0];
    assert_eq!(hit.event_id, event.id);
    assert_eq!(hit.history_record_id, Some(record.id));
    assert_eq!(hit.preview, "projection duplicate oracle");
    assert_eq!(hit.provider, Some(CaptureProvider::Codex));
    assert_eq!(hit.history_source.as_deref(), Some("test-plugin/history"));
    assert_eq!(hit.history_source_plugin.as_deref(), Some("test-plugin"));
    assert_eq!(hit.provider_key.as_deref(), Some("provider-7"));
    assert_eq!(hit.source_id.as_deref(), Some("source-7"));
    assert_eq!(hit.source_format.as_deref(), Some("test-jsonl"));
    assert_eq!(hit.cwd.as_deref(), Some("/workspace/ctx"));
    assert_eq!(
        hit.raw_source_path.as_deref(),
        Some("/history/session.jsonl")
    );
    assert_eq!(hit.cursor.as_deref(), Some("cursor-7"));
    assert_eq!(
        hit.record_title.as_deref(),
        Some("Projection metadata title")
    );
    assert_eq!(hit.record_kind.as_deref(), Some("task"));
    assert_eq!(
        hit.record_workspace.as_deref(),
        Some("/workspace/multilingual")
    );
}

#[test]
fn lexical_event_search_pagination_is_a_stable_slice_of_ranked_results() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let events = [
        with_occurred_at(local_preview_event(1, "paginationoracle"), 0),
        with_occurred_at(local_preview_event(2, "paginationoracle"), 3),
        with_occurred_at(local_preview_event(3, "paginationoracle"), 1),
        with_occurred_at(local_preview_event(4, "paginationoracle"), 2),
    ];
    for event in events.iter().rev() {
        store.upsert_event(event).unwrap();
    }

    let all = store.search_event_hits("paginationoracle", 10).unwrap();
    let page = store
        .search_event_hits_page("paginationoracle", 2, 1)
        .unwrap();
    assert_eq!(
        all.iter().map(|hit| hit.event_id).collect::<Vec<_>>(),
        vec![events[1].id, events[3].id, events[2].id, events[0].id]
    );
    assert_eq!(page, all[1..3]);
}

#[test]
fn lexical_event_search_plan_scans_fts_once_per_match_clause() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let clauses = fts_match_clauses("planalpha planbeta");
    let expected_scans = clauses.len();
    let (sql, values) = lexical_event_search_query(clauses, 10, 0, false);
    let mut stmt = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let details = stmt
        .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let virtual_table_scans = details
        .iter()
        .filter(|detail| detail.contains("event_search VIRTUAL TABLE"))
        .count();

    assert_eq!(virtual_table_scans, expected_scans, "{details:#?}");
}
