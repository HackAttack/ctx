use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result as AnyResult};
use ctx_history_core::{
    CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    EventSearchFilters, GenerationWriter, SearchContentScope, VerifiedIndex, WriterOptions,
};
use serde_json::json;

use super::*;
use crate::{RefreshArg, SearchBackendArg};

use super::super::query::resolve_source_search_backend_with_port;
use super::super::{collect_search_hits_with_port, SourceSearchFailure, SourceSearchRequest};

#[derive(Clone, Default)]
struct CallLog(Arc<Mutex<Vec<String>>>);

impl CallLog {
    fn push(&self, value: impl Into<String>) {
        self.0
            .lock()
            .expect("fake semantic port call log must remain available")
            .push(value.into());
    }

    fn values(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("fake semantic port call log must remain available")
            .clone()
    }
}

struct FakeSemanticPort {
    calls: CallLog,
    begin_error: Option<HistorySemanticError>,
}

impl FakeSemanticPort {
    fn ready(calls: CallLog) -> Self {
        Self {
            calls,
            begin_error: None,
        }
    }

    fn failing(calls: CallLog, error: HistorySemanticError) -> Self {
        Self {
            calls,
            begin_error: Some(error),
        }
    }
}

impl HistorySemanticPort for FakeSemanticPort {
    type Query<'a> = FakeSemanticQuery;

    fn capability(&self) -> SemanticCapability {
        self.calls.push("capability");
        SemanticCapability::Available
    }

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        self.calls.push("begin_query");
        if let Some(error) = self.begin_error.as_ref() {
            return Err(error.clone());
        }
        Ok(FakeSemanticQuery {
            calls: self.calls.clone(),
        })
    }
}

struct FakeSemanticQuery {
    calls: CallLog,
}

impl HistorySemanticQuery for FakeSemanticQuery {
    fn candidates(
        &mut self,
        query: &str,
        _filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        self.calls
            .push(format!("candidates:{query}:{candidate_limit}"));
        Ok(HistorySemanticBatch {
            candidates: Vec::new(),
            diagnostics: json!({"fake_query": query}),
        })
    }
}

fn empty_index(root: &Path) -> AnyResult<VerifiedIndex> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("semantic-port-empty.jsonl")?,
        )?,
    )?;
    let index_root = root.join("index");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())?
        .into_writer()
        .map_err(|recovery| {
            anyhow!(
                "semantic-port test index requires recovery for generation {}: {}",
                recovery.generation_id(),
                recovery.detail()
            )
        })?;
    writer.begin_source(source.clone())?;
    let observation = SourceObservation::new(source, "regular-file-v1", vec![1])?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "semantic-port-test-parser-v1",
        [1; 32],
        ScannedSourceCounts::default(),
    )?)?;
    writer.commit(|_| true)?;
    Ok(VerifiedIndex::open_pinned(&index_root)?)
}

fn semantic_request(backend: SearchBackendArg) -> SourceSearchRequest {
    SourceSearchRequest {
        query: "first query".to_owned(),
        terms: vec!["second query".to_owned()],
        limit: 10,
        provider: None,
        history_source: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        workspace: None,
        since: None,
        primary_only: false,
        include_subagents: false,
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        events: false,
        include_current_session: true,
        backend: Some(backend),
        semantic_weight: 0.35,
        semantic_enabled: true,
        semantic_daemon_enabled: true,
        refresh: RefreshArg::Off,
    }
}

#[test]
fn fake_port_begins_once_before_ordered_query_calls() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let request = semantic_request(SearchBackendArg::Hybrid);

    let collection = collect_search_hits_with_port(
        &request,
        &index,
        temp.path(),
        request.semantic_weight,
        &EventSearchFilters::default(),
        &port,
    )?;

    assert_eq!(collection.semantic_status, "ready");
    assert_eq!(
        collection.semantic_diagnostics.as_ref().unwrap()["query_count"],
        2
    );
    assert_eq!(
        calls.values(),
        vec![
            "begin_query",
            "candidates:first query:1600",
            "candidates:second query:1600",
        ]
    );
    Ok(())
}

#[test]
fn backend_resolution_reads_capability_through_the_consumer_port() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    crate::config::set_semantic_search_enabled(temp.path(), true)?;
    crate::config::set_daemon_enabled(temp.path(), true)?;
    let config = crate::config::AppConfig::load(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let request = semantic_request(SearchBackendArg::Semantic);

    let backend = resolve_source_search_backend_with_port(&request, &config, &port)?;

    assert_eq!(backend, SearchBackendArg::Semantic);
    assert_eq!(calls.values(), vec!["capability"]);
    Ok(())
}

#[test]
fn semantic_only_preserves_the_consumer_owned_typed_error() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::failing(
        calls.clone(),
        HistorySemanticError::not_ready("semantic_fixture_not_ready", "fixture unavailable", true),
    );
    let request = semantic_request(SearchBackendArg::Semantic);

    let error = collect_search_hits_with_port(
        &request,
        &index,
        temp.path(),
        request.semantic_weight,
        &EventSearchFilters::default(),
        &port,
    )
    .expect_err("semantic-only search must surface the typed port error");
    let SourceSearchFailure::Semantic(typed) = error else {
        panic!("semantic-only search must preserve the typed port failure");
    };

    assert_eq!(typed.code(), "semantic_fixture_not_ready");
    assert_eq!(typed.detail(), "fixture unavailable");
    assert!(typed.retryable());
    assert_eq!(calls.values(), vec!["begin_query"]);
    Ok(())
}

#[test]
fn hybrid_maps_typed_port_failure_to_the_existing_lexical_fallback() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::failing(
        calls.clone(),
        HistorySemanticError::failed("fixture transport failed"),
    );
    let request = semantic_request(SearchBackendArg::Hybrid);

    let collection = collect_search_hits_with_port(
        &request,
        &index,
        temp.path(),
        request.semantic_weight,
        &EventSearchFilters::default(),
        &port,
    )?;

    assert_eq!(collection.effective_backend, SearchBackendArg::Lexical);
    assert_eq!(collection.semantic_status, "unavailable");
    let fallback = collection
        .semantic_fallback
        .expect("hybrid failure must retain fallback diagnostics");
    assert_eq!(fallback.code, "semantic_query_failed");
    assert_eq!(fallback.detail, "fixture transport failed");
    assert_eq!(calls.values(), vec!["begin_query"]);
    Ok(())
}

#[test]
fn zero_weight_hybrid_never_opens_the_semantic_port() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let mut request = semantic_request(SearchBackendArg::Hybrid);
    request.semantic_weight = 0.0;

    let collection = collect_search_hits_with_port(
        &request,
        &index,
        temp.path(),
        request.semantic_weight,
        &EventSearchFilters::default(),
        &port,
    )?;

    assert_eq!(collection.semantic_status, "skipped");
    assert!(calls.values().is_empty());
    Ok(())
}
