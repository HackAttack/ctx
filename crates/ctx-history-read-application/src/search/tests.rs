use super::*;

fn ancestry(
    session_id: u128,
    parent_session_id: Option<u128>,
    claimed_root_session_id: Option<u128>,
) -> SessionAncestry {
    SessionAncestry {
        session_id: Uuid::from_u128(session_id),
        parent_session_id: parent_session_id.map(Uuid::from_u128),
        claimed_root_session_id: claimed_root_session_id.map(Uuid::from_u128),
    }
}

fn resolved_test_root(
    sessions: &[SessionAncestry],
    records: &BTreeMap<Uuid, SessionAncestry>,
) -> Option<Uuid> {
    resolved_unique_session_tree_root_id(sessions, |session_id| {
        Ok(records.get(&session_id).copied())
    })
    .unwrap()
}

fn linear_ancestry(depth: usize) -> (SessionAncestry, Uuid, BTreeMap<Uuid, SessionAncestry>) {
    let records = (0..=depth)
        .map(|position| {
            let session_id = 1_000 + position as u128;
            let parent_session_id = (position < depth).then_some(session_id + 1);
            let claimed_root_session_id = parent_session_id.or(Some(session_id));
            ancestry(session_id, parent_session_id, claimed_root_session_id)
        })
        .collect::<Vec<_>>();
    let active = records[0];
    let root_id = records[depth].session_id;
    let records = records
        .into_iter()
        .map(|record| (record.session_id, record))
        .collect();
    (active, root_id, records)
}

fn request() -> SearchRequest {
    SearchRequest {
        query: "  first query  ".to_owned(),
        terms: vec![" second query ".to_owned(), " ".to_owned()],
        limit: 20,
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
        include_current_session: false,
        backend: Some(SearchBackend::Lexical),
        semantic_weight: 0.35,
    }
}

#[test]
fn normalized_query_preserves_typed_argument_order() {
    let query = NormalizedSearchQuery::from_request(&request());
    assert_eq!(query.texts(), vec!["first query", "second query"]);
    assert_eq!(query.display(), "first query OR second query");
    assert_eq!(query.positional(), Some("first query"));
    assert_eq!(query.terms(), &["second query"]);
}

#[test]
fn custom_source_filter_rejects_noncustom_provider() {
    let mut request = request();
    request.history_source = Some("plugin/source".to_owned());
    request.provider = Some(CaptureProvider::Claude);
    assert_eq!(
        validate_search_request(&request).unwrap_err().to_string(),
        "custom history source filters require the custom provider"
    );
}

#[test]
fn unsupported_semantic_scope_remains_typed() {
    let mut request = request();
    request.backend = Some(SearchBackend::Semantic);
    request.content_scope = SearchContentScope::Outputs;
    let error = unsupported_semantic_scope(&request).unwrap();
    assert_eq!(
        error.reason(),
        Some(SemanticReason::ContentScopeUnsupported)
    );
    assert!(!error.retryable());
}

#[test]
fn explicit_session_selection_overrides_only_the_default_agent_scope() {
    let mut request = request();
    assert_eq!(
        search_agent_scope(&request, None),
        SearchAgentScope::Primary
    );
    assert_eq!(
        search_agent_scope(&request, Some(Uuid::nil())),
        SearchAgentScope::All
    );
    request.primary_only = true;
    assert_eq!(
        search_agent_scope(&request, Some(Uuid::nil())),
        SearchAgentScope::Primary
    );
}

#[test]
fn active_tree_root_resolves_a_direct_child() {
    let root = ancestry(1, None, Some(1));
    let child = ancestry(2, Some(1), Some(1));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(
        resolved_test_root(&[child], &records),
        Some(root.session_id)
    );
}

#[test]
fn active_tree_root_resolves_a_grandchild_with_an_immediate_parent_claim() {
    let root = ancestry(1, None, None);
    let child = ancestry(2, Some(1), Some(1));
    let grandchild = ancestry(3, Some(2), Some(2));
    let records = BTreeMap::from([(root.session_id, root), (child.session_id, child)]);
    assert_eq!(
        resolved_test_root(&[grandchild], &records),
        Some(root.session_id)
    );
}

#[test]
fn active_tree_root_rejects_a_malformed_claimed_root() {
    let root = ancestry(1, None, None);
    let child = ancestry(2, Some(1), Some(99));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(resolved_test_root(&[child], &records), None);
}

#[test]
fn active_tree_root_rejects_ambiguous_provider_session_matches() {
    let root = ancestry(1, None, None);
    let first = ancestry(2, Some(1), Some(1));
    let second = ancestry(3, Some(1), Some(1));
    let records = BTreeMap::from([(root.session_id, root)]);
    assert_eq!(resolved_test_root(&[first, second], &records), None);
}

#[test]
fn active_tree_root_rejects_a_missing_parent() {
    let child = ancestry(2, Some(1), Some(1));
    assert_eq!(resolved_test_root(&[child], &BTreeMap::new()), None);
}

#[test]
fn active_tree_root_rejects_a_parent_cycle() {
    let first = ancestry(1, Some(2), Some(2));
    let second = ancestry(2, Some(1), Some(1));
    let records = BTreeMap::from([(first.session_id, first), (second.session_id, second)]);
    assert_eq!(resolved_test_root(&[first], &records), None);
}

#[test]
fn active_tree_root_rejects_depth_over_64() {
    let (at_limit, root_id, records) = linear_ancestry(MAX_ACTIVE_SESSION_ANCESTORS);
    assert_eq!(resolved_test_root(&[at_limit], &records), Some(root_id));
    let (over_limit, _, records) = linear_ancestry(MAX_ACTIVE_SESSION_ANCESTORS + 1);
    assert_eq!(resolved_test_root(&[over_limit], &records), None);
}

#[test]
fn weighted_rrf_keeps_exact_endpoint_weights() {
    assert_eq!(weighted_rrf_score(Some(1), None, 0.0), 1.0 / 61.0);
    assert_eq!(weighted_rrf_score(None, Some(1), 1.0), 1.0 / 61.0);
    assert_eq!(weighted_rrf_score(Some(1), None, 1.0), 0.0);
}
