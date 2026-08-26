use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{bytes_bucket, count_bucket, RefreshStatus, SearchBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStopReason {
    Decisive,
    Exhausted,
    CandidateCap,
    FixedPool,
}

impl SearchStopReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Decisive => "decisive",
            Self::Exhausted => "exhausted",
            Self::CandidateCap => "candidate_cap",
            Self::FixedPool => "fixed_pool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFailurePhase {
    Preparation,
    Refresh,
    GenerationOpen,
    QueryPreparation,
    SemanticRetrieval,
    IndexQueryDecode,
    ResultProjection,
    Render,
    Output,
}

impl SearchFailurePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Preparation => "preparation",
            Self::Refresh => "refresh",
            Self::GenerationOpen => "generation_open",
            Self::QueryPreparation => "query_preparation",
            Self::SemanticRetrieval => "semantic_retrieval",
            Self::IndexQueryDecode => "index_query_decode",
            Self::ResultProjection => "result_projection",
            Self::Render => "render",
            Self::Output => "output",
        }
    }
}

/// Missing and derived search-health facts serialized only on the terminal event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchHealthFacts {
    pub retrieval_rounds: Option<u64>,
    pub query_executions: Option<u64>,
    pub candidate_rows: Option<u64>,
    pub records_decoded: Option<u64>,
    pub encoded_core_bytes_decoded: Option<u64>,
    pub final_candidate_pool: Option<u64>,
    pub candidate_pool_truncated: Option<bool>,
    pub stop_reason: Option<SearchStopReason>,
    pub failure_phase: Option<SearchFailurePhase>,
}

/// Exact search fields attached to an MCP terminal event before serialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchTerminalFacts {
    pub refresh_duration: Option<Duration>,
    pub refresh_status: Option<RefreshStatus>,
    pub refresh_source_count: Option<u64>,
    pub query_duration: Option<Duration>,
    pub backend_requested: Option<SearchBackend>,
    pub backend_effective: Option<SearchBackend>,
    pub health: SearchHealthFacts,
    pub output_duration: Option<Duration>,
    pub output_served: Option<bool>,
}

impl SearchHealthFacts {
    pub(crate) fn insert_properties(self, properties: &mut Map<String, Value>) {
        insert_count(
            properties,
            "search_retrieval_round_count_bucket",
            self.retrieval_rounds,
        );
        insert_count(
            properties,
            "search_query_execution_count_bucket",
            self.query_executions,
        );
        insert_count(
            properties,
            "search_candidate_rows_total_bucket",
            self.candidate_rows,
        );
        insert_count(
            properties,
            "search_candidate_records_decoded_bucket",
            self.records_decoded,
        );
        if let Some(value) = self.encoded_core_bytes_decoded {
            properties.insert(
                "search_candidate_core_bytes_decoded_bucket".to_owned(),
                json!(bytes_bucket(value).as_str()),
            );
        }
        insert_count(
            properties,
            "search_final_candidate_pool_bucket",
            self.final_candidate_pool,
        );
        if let Some(value) = self.candidate_pool_truncated {
            properties.insert("search_candidate_pool_truncated".to_owned(), json!(value));
        }
        if let Some(value) = self.stop_reason {
            properties.insert("search_stop_reason".to_owned(), json!(value.as_str()));
        }
        if let Some(value) = self.failure_phase {
            properties.insert("search_failure_phase".to_owned(), json!(value.as_str()));
        }
    }
}

fn insert_count(properties: &mut Map<String, Value>, name: &str, value: Option<u64>) {
    if let Some(value) = value {
        properties.insert(name.to_owned(), json!(count_bucket(value).as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_health_facts_are_bucketed_only_at_serialization() {
        let health = SearchHealthFacts {
            candidate_rows: Some(21),
            encoded_core_bytes_decoded: Some(102_400),
            final_candidate_pool: Some(2),
            ..SearchHealthFacts::default()
        };
        let mut properties = Map::new();
        health.insert_properties(&mut properties);

        assert_eq!(properties["search_candidate_rows_total_bucket"], "21-100");
        assert_eq!(
            properties["search_candidate_core_bytes_decoded_bucket"],
            "100kb-1mb"
        );
        assert_eq!(properties["search_final_candidate_pool_bucket"], "2-5");
    }
}
