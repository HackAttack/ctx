use super::*;
use crate::records::stored_event_record_with_size;

impl EventCandidateQueryReceipt {
    fn record_query_execution(&mut self) -> Result<()> {
        self.query_executions = self
            .query_executions
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_collector_hits(&mut self, hits: usize) -> Result<()> {
        let hits = u64::try_from(hits).map_err(|_| IndexError::CountOverflow)?;
        self.collector_hits = self
            .collector_hits
            .checked_add(hits)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_decoded(&mut self, encoded_core_bytes: usize) -> Result<()> {
        let encoded_core_bytes =
            u64::try_from(encoded_core_bytes).map_err(|_| IndexError::CountOverflow)?;
        let records_decoded = self
            .records_decoded
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        let encoded_core_bytes_decoded = self
            .encoded_core_bytes_decoded
            .checked_add(encoded_core_bytes)
            .ok_or(IndexError::CountOverflow)?;
        self.records_decoded = records_decoded;
        self.encoded_core_bytes_decoded = encoded_core_bytes_decoded;
        Ok(())
    }
}

impl VerifiedIndex {
    /// Searches full policy-selected event text using ordinary analyzed text.
    ///
    /// An analyzed token admits a partial match. Full query-term coverage ranks
    /// ahead of partial coverage, followed by ordinary lexical relevance.
    /// QueryParser operators and field syntax are intentionally not accepted.
    pub fn search_event_candidates(
        &self,
        natural_text: &str,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_any_with_filters_diagnosed(
            &[natural_text],
            &EventSearchFilters::default(),
            limit,
        )
        .map(|observed| observed.candidates)
        .map_err(|failure| failure.error)
    }

    /// Searches policy-selected event text with conjunctive metadata filters.
    ///
    /// Exact-value fields use their stored spelling. Workspace and file filters
    /// use case-insensitive substring matching over provider-declared literal
    /// facts without interpreting those facts as repository authority.
    pub fn search_event_candidates_with_filters(
        &self,
        natural_text: &str,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_any_with_filters_diagnosed(&[natural_text], filters, limit)
            .map(|observed| observed.candidates)
            .map_err(|failure| failure.error)
    }

    /// Searches OR-composed natural-text alternatives with shared filters.
    ///
    /// Matching any unique analyzed token admits the event. Results rank by
    /// query-term coverage before ordinary lexical relevance. This is the
    /// indexed implementation of the CLI's repeated `--term` contract.
    pub fn search_event_candidates_any_with_filters(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.search_event_candidates_any_with_filters_diagnosed(natural_texts, filters, limit)
            .map(|observed| observed.candidates)
            .map_err(|failure| failure.error)
    }

    pub fn search_event_candidates_any_with_filters_diagnosed(
        &self,
        natural_texts: &[&str],
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedEventCandidateQueryResult {
        #[cfg(any(test, feature = "test-support"))]
        let _failure_injection_reset = lexical_candidate_materialization_failure_reset();
        let mut receipt = EventCandidateQueryReceipt::default();
        let failure = |error, receipt| Box::new(EventCandidateQueryFailure { error, receipt });
        validate_lexical_result_limit(limit).map_err(|error| failure(error, receipt))?;
        LEXICAL_QUERY_LIMITS
            .validate_texts(natural_texts.iter().copied())
            .map_err(|error| failure(error, receipt))?;
        filters
            .validate_content_scope()
            .map_err(|error| failure(error, receipt))?;
        if limit == 0 {
            return Ok(ObservedEventSearchCandidates::default());
        }
        let fields =
            fields_from_schema(self.searcher.schema()).map_err(|error| failure(error, receipt))?;
        let ranking_terms = self
            .body_query_terms(natural_texts, fields)
            .map_err(|error| failure(error, receipt))?;
        if ranking_terms.is_empty() {
            return Ok(ObservedEventSearchCandidates::default());
        }
        let result = (|| {
            let mut candidates = Vec::with_capacity(limit);
            let mut seen = BTreeSet::new();
            if ranking_terms.len() == 1 {
                #[cfg(any(test, feature = "test-support"))]
                record_lexical_query_construction();
                let body_query =
                    class_weighted_body_query(&ranking_terms, 1, filters.content_scope, fields);
                let lexical_limit = limit
                    .checked_add(seen.len())
                    .ok_or(IndexError::CountOverflow)?;
                for candidate in self.collect_event_candidate_addresses(
                    body_query,
                    filters,
                    lexical_limit,
                    fields,
                    &mut receipt,
                )? {
                    if seen.insert(candidate.event_id) {
                        candidates.push(candidate);
                        if candidates.len() == limit {
                            break;
                        }
                    }
                }
                return self.materialize_event_candidates(candidates, fields, &mut receipt);
            }

            // Minimum-should-match coverage tiers keep work bounded by the
            // caller's candidate request rather than corpus term frequency.
            for minimum_required in (1..=ranking_terms.len()).rev() {
                #[cfg(any(test, feature = "test-support"))]
                record_lexical_query_construction();
                let body_query = class_weighted_body_query(
                    &ranking_terms,
                    minimum_required,
                    filters.content_scope,
                    fields,
                );
                let tier_limit = limit
                    .checked_add(seen.len())
                    .ok_or(IndexError::CountOverflow)?;
                for candidate in self.collect_event_candidate_addresses(
                    body_query,
                    filters,
                    tier_limit,
                    fields,
                    &mut receipt,
                )? {
                    if seen.insert(candidate.event_id) {
                        candidates.push(candidate);
                        if candidates.len() == limit {
                            return self.materialize_event_candidates(
                                candidates,
                                fields,
                                &mut receipt,
                            );
                        }
                    }
                }
            }
            self.materialize_event_candidates(candidates, fields, &mut receipt)
        })();
        match result {
            Ok(candidates) => Ok(ObservedEventSearchCandidates {
                candidates,
                receipt,
            }),
            Err(error) => Err(failure(error, receipt)),
        }
    }

    /// Lists filtered metadata records without requiring a lexical term.
    pub fn list_event_candidates_with_filters(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> Result<Vec<EventSearchCandidate>> {
        self.list_event_candidates_with_filters_diagnosed(filters, limit)
            .map(|observed| observed.candidates)
            .map_err(|failure| failure.error)
    }

    pub fn list_event_candidates_with_filters_diagnosed(
        &self,
        filters: &EventSearchFilters,
        limit: usize,
    ) -> DiagnosedEventCandidateQueryResult {
        #[cfg(any(test, feature = "test-support"))]
        let _failure_injection_reset = lexical_candidate_materialization_failure_reset();
        let mut receipt = EventCandidateQueryReceipt::default();
        let failure = |error, receipt| Box::new(EventCandidateQueryFailure { error, receipt });
        validate_lexical_result_limit(limit).map_err(|error| failure(error, receipt))?;
        filters
            .validate_content_scope()
            .map_err(|error| failure(error, receipt))?;
        if limit == 0 {
            return Ok(ObservedEventSearchCandidates::default());
        }
        let fields =
            fields_from_schema(self.searcher.schema()).map_err(|error| failure(error, receipt))?;
        let result = self
            .collect_event_candidate_addresses(
                Box::new(AllQuery),
                filters,
                limit,
                fields,
                &mut receipt,
            )
            .and_then(|candidates| {
                self.materialize_event_candidates(candidates, fields, &mut receipt)
            });
        match result {
            Ok(candidates) => Ok(ObservedEventSearchCandidates {
                candidates,
                receipt,
            }),
            Err(error) => Err(failure(error, receipt)),
        }
    }

    /// Selects semantic-eligible event IDs with the exact metadata predicate
    /// used by lexical search, bound to this immutable generation.
    ///
    /// Selection reads indexed postings and event-ID fast fields only. It does
    /// not decode stored Core records or reopen provider sources.
    pub fn semantic_filter_projection(
        &self,
        filters: &EventSearchFilters,
    ) -> Result<SemanticFilterProjection> {
        filters.validate_content_scope()?;
        validate_event_sort_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let semantic_eligibility = Box::new(BooleanQuery::intersection(vec![
            Box::new(TermQuery::new(
                Term::from_field_text(fields.event_type, "message"),
                IndexRecordOption::Basic,
            )),
            Box::new(TermQuery::new(
                Term::from_field_text(fields.role, "user"),
                IndexRecordOption::Basic,
            )),
        ]));
        let source_identity_query = self.source_identity_query(filters, fields)?;
        let query =
            filtered_event_query(semantic_eligibility, source_identity_query, filters, fields)?;
        let addresses = self
            .searcher
            .search(query.as_ref(), &DocSetCollector)
            .map_err(IndexError::from)?;
        let mut event_ids = HashSet::with_capacity(addresses.len());
        for address in addresses {
            let (event_id, _, _) = core_event_fast_preflight(&self.searcher, address)?;
            if !event_ids.insert(event_id) {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        Ok(SemanticFilterProjection {
            generation_id: self.generation_id.clone(),
            event_ids,
        })
    }

    fn collect_event_candidate_addresses(
        &self,
        body_query: Box<dyn Query>,
        filters: &EventSearchFilters,
        limit: usize,
        fields: Fields,
        receipt: &mut EventCandidateQueryReceipt,
    ) -> Result<Vec<LexicalAddressCandidate>> {
        validate_event_sort_fast_fields(&self.searcher)?;
        let source_identity_query = self.source_identity_query(filters, fields)?;
        let query = filtered_event_query(body_query, source_identity_query, filters, fields)?;
        let collector = TopDocs::with_limit(limit).tweak_score(move |segment_reader| {
            // These readers were checked above. The fallbacks keep this
            // infallible collector closure panic-free if Tantivy ever changes
            // when it resolves a validated fast field.
            let high = segment_reader
                .fast_fields()
                .u64(EVENT_ID_HIGH_FIELD)
                .ok()
                .map(|column| column.first_or_default_col(0));
            let low = segment_reader
                .fast_fields()
                .u64(EVENT_ID_LOW_FIELD)
                .ok()
                .map(|column| column.first_or_default_col(0));
            move |doc, score| {
                let high = high.as_ref().map_or(0, |column| column.get_val(doc));
                let low = low.as_ref().map_or(0, |column| column.get_val(doc));
                (score, Reverse((high, low)))
            }
        });
        type ScoredDocAddress = ((Score, Reverse<(u64, u64)>), DocAddress);
        receipt.record_query_execution()?;
        #[cfg(any(test, feature = "test-support"))]
        record_lexical_query_execution();
        let hits: Vec<ScoredDocAddress> = self.searcher.search(query.as_ref(), &collector)?;
        receipt.record_collector_hits(hits.len())?;
        let mut candidates = Vec::with_capacity(hits.len());
        for ((score, Reverse((event_id_high, event_id_low))), address) in hits {
            candidates.push(LexicalAddressCandidate {
                event_id: Uuid::from_u128(
                    (u128::from(event_id_high) << 64) | u128::from(event_id_low),
                ),
                address,
                score,
            });
        }
        Ok(candidates)
    }

    fn materialize_event_candidates(
        &self,
        candidates: Vec<LexicalAddressCandidate>,
        fields: Fields,
        receipt: &mut EventCandidateQueryReceipt,
    ) -> Result<Vec<EventSearchCandidate>> {
        let mut materialized = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            #[cfg(any(test, feature = "test-support"))]
            if lexical_candidate_materialization_should_fail() {
                return Err(IndexError::InvalidStoredDocumentField(
                    "test_lexical_candidate_materialization_failure",
                ));
            }
            let (event, encoded_core_bytes) =
                stored_event_record_with_size(&self.searcher, candidate.address, fields)?;
            receipt.record_decoded(encoded_core_bytes)?;
            if event.event_id.as_uuid() != candidate.event_id {
                return Err(IndexError::InvalidStoredDocumentField("event_id"));
            }
            materialized.push(EventSearchCandidate {
                event,
                score: candidate.score,
            });
        }
        Ok(materialized)
    }

    fn source_identity_query(
        &self,
        filters: &EventSearchFilters,
        fields: Fields,
    ) -> Result<Option<Box<dyn Query>>> {
        if !filters.has_source_identity_filter() {
            return Ok(None);
        }
        filters.validate_source_identity_filters()?;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.provider, "custom"),
                IndexRecordOption::Basic,
            )),
        )];
        if let Some(history_source) = filters.history_source.as_deref() {
            let Some((history_provider_key, history_source_id)) =
                history_source.trim().split_once('/')
            else {
                return Ok(Some(Box::new(EmptyQuery)));
            };
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, history_provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, history_source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(provider_key) = filters.provider_key.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(source_id) = filters.source_id.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }
}

fn class_weighted_body_query(
    ranking_terms: &[Term],
    minimum_required: usize,
    scope: SearchContentScope,
    fields: Fields,
) -> Box<dyn Query> {
    match scope {
        SearchContentScope::All | SearchContentScope::Transcript => {
            Box::new(ClassWeightedQuery::new(
                ordinary_body_query(ranking_terms, minimum_required),
                fields.event_type,
                scope,
            ))
        }
        SearchContentScope::Calls | SearchContentScope::Outputs => {
            ordinary_body_query(ranking_terms, minimum_required)
        }
    }
}

fn ordinary_body_query(ranking_terms: &[Term], minimum_required: usize) -> Box<dyn Query> {
    if ranking_terms.len() == 1 {
        return Box::new(TermQuery::new(
            ranking_terms[0].clone(),
            IndexRecordOption::WithFreqs,
        ));
    }
    let alternatives = ranking_terms
        .iter()
        .cloned()
        .map(|term| Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>)
        .collect();
    Box::new(BooleanQuery::union_with_minimum_required_clauses(
        alternatives,
        minimum_required,
    ))
}

#[derive(Debug)]
struct ClassWeightedQuery {
    inner: Box<dyn Query>,
    event_type_field: Field,
    scope: SearchContentScope,
}

impl ClassWeightedQuery {
    fn new(inner: Box<dyn Query>, event_type_field: Field, scope: SearchContentScope) -> Self {
        debug_assert!(matches!(
            scope,
            SearchContentScope::All | SearchContentScope::Transcript
        ));
        Self {
            inner,
            event_type_field,
            scope,
        }
    }
}

impl Clone for ClassWeightedQuery {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.box_clone(),
            event_type_field: self.event_type_field,
            scope: self.scope,
        }
    }
}

impl Query for ClassWeightedQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(ClassWeightedWeight {
            inner: self.inner.weight(enable_scoring)?,
            event_type_field: self.event_type_field,
            scope: self.scope,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.inner.query_terms(visitor);
    }
}

struct ClassWeightedWeight {
    inner: Box<dyn Weight>,
    event_type_field: Field,
    scope: SearchContentScope,
}

impl ClassWeightedWeight {
    fn class_postings(&self, reader: &SegmentReader) -> tantivy::Result<ClassPostings> {
        ClassPostings::open(reader, self.event_type_field, self.scope)
    }
}

impl Weight for ClassWeightedWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(ClassWeightedScorer {
            inner: self.inner.scorer(reader, boost)?,
            classes: self.class_postings(reader)?,
        }))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let inner_explanation = self.inner.explain(reader, doc)?;
        let mut scorer = self.scorer(reader, 1.0)?;
        scorer.seek(doc);
        let mut explanation = Explanation::new("event-content class weight", scorer.score());
        explanation.add_detail(inner_explanation);
        Ok(explanation)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        self.inner.count(reader)
    }

    fn for_each(
        &self,
        reader: &SegmentReader,
        callback: &mut dyn FnMut(DocId, Score),
    ) -> tantivy::Result<()> {
        let mut classes = self.class_postings(reader)?;
        self.inner.for_each(reader, &mut |doc, score| {
            callback(doc, score * classes.weight(doc))
        })
    }

    fn for_each_no_score(
        &self,
        reader: &SegmentReader,
        callback: &mut dyn FnMut(&[DocId]),
    ) -> tantivy::Result<()> {
        self.inner.for_each_no_score(reader, callback)
    }

    fn for_each_pruning(
        &self,
        threshold: Score,
        reader: &SegmentReader,
        callback: &mut dyn FnMut(DocId, Score) -> Score,
    ) -> tantivy::Result<()> {
        let mut classes = self.class_postings(reader)?;
        let mut weighted_threshold = threshold;
        // Every class weight is at most 1.0, so an unweighted score at or
        // below the collector threshold can never become competitive. The
        // inner query may therefore retain its native Block-WAND pruning.
        self.inner
            .for_each_pruning(threshold, reader, &mut |doc, score| {
                let weighted_score = score * classes.weight(doc);
                if weighted_score > weighted_threshold {
                    weighted_threshold = callback(doc, weighted_score);
                }
                weighted_threshold
            })
    }
}

struct ClassWeightedScorer {
    inner: Box<dyn Scorer>,
    classes: ClassPostings,
}

impl DocSet for ClassWeightedScorer {
    fn advance(&mut self) -> DocId {
        self.inner.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.inner.seek(target)
    }

    fn doc(&self) -> DocId {
        self.inner.doc()
    }

    fn size_hint(&self) -> u32 {
        self.inner.size_hint()
    }

    fn cost(&self) -> u64 {
        self.inner.cost()
    }
}

impl Scorer for ClassWeightedScorer {
    fn score(&mut self) -> Score {
        self.inner.score() * self.classes.weight(self.inner.doc())
    }
}

struct ClassPostings {
    message: SegmentPostings,
    summary: SegmentPostings,
    outputs: Vec<SegmentPostings>,
    scope: SearchContentScope,
}

impl ClassPostings {
    fn open(
        reader: &SegmentReader,
        event_type_field: Field,
        scope: SearchContentScope,
    ) -> tantivy::Result<Self> {
        let inverted_index = reader.inverted_index(event_type_field)?;
        let postings = |event_type: &str| {
            inverted_index
                .read_postings(
                    &Term::from_field_text(event_type_field, event_type),
                    IndexRecordOption::Basic,
                )
                .map(|postings| postings.unwrap_or_else(SegmentPostings::empty))
        };
        Ok(Self {
            message: postings("message")?,
            summary: postings("summary")?,
            outputs: if scope == SearchContentScope::All {
                OUTPUT_EVENT_TYPES
                    .iter()
                    .map(|event_type| postings(event_type))
                    .collect::<std::io::Result<Vec<_>>>()?
            } else {
                Vec::new()
            },
            scope,
        })
    }

    fn weight(&mut self, doc: DocId) -> Score {
        if posting_matches(&mut self.message, doc) {
            return 1.0;
        }
        if posting_matches(&mut self.summary, doc) {
            return 0.9;
        }
        if self
            .outputs
            .iter_mut()
            .any(|postings| posting_matches(postings, doc))
        {
            return 0.6;
        }
        match self.scope {
            SearchContentScope::All => 0.8,
            SearchContentScope::Transcript => 0.9,
            SearchContentScope::Calls | SearchContentScope::Outputs => unreachable!(),
        }
    }
}

fn posting_matches(postings: &mut SegmentPostings, doc: DocId) -> bool {
    let posting_doc = postings.doc();
    (posting_doc == doc) || (posting_doc < doc && postings.seek(doc) == doc)
}

#[derive(Debug, Clone, Copy)]
struct LexicalAddressCandidate {
    event_id: Uuid,
    address: DocAddress,
    score: Score,
}
