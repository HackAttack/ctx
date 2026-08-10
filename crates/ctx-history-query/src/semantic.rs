use ctx_history_index_query::{EventSearchCandidate, EventSearchFilters, VerifiedIndex};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticCapability {
    Available,
    Unavailable,
}

#[derive(Debug)]
pub struct HistorySemanticBatch {
    pub candidates: Vec<EventSearchCandidate>,
    pub diagnostics: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HistorySemanticError {
    #[error("source-backed semantic search is not ready ({code}): {detail}")]
    NotReady {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
    #[error("{detail}")]
    Failed { detail: String },
}

impl HistorySemanticError {
    pub fn not_ready(code: &'static str, detail: impl Into<String>, retryable: bool) -> Self {
        Self::NotReady {
            code,
            detail: detail.into(),
            retryable,
        }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotReady { code, .. } => code,
            Self::Failed { .. } => "semantic_query_failed",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::NotReady { detail, .. } | Self::Failed { detail } => detail,
        }
    }

    pub const fn retryable(&self) -> bool {
        match self {
            Self::NotReady { retryable, .. } => *retryable,
            Self::Failed { .. } => false,
        }
    }
}

pub trait HistorySemanticQuery {
    fn candidates(
        &mut self,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError>;
}

pub trait HistorySemanticPort: Send + Sync {
    type Query<'a>: HistorySemanticQuery + 'a
    where
        Self: 'a;

    fn capability(&self) -> SemanticCapability;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError>;
}

#[cfg(test)]
mod tests;
