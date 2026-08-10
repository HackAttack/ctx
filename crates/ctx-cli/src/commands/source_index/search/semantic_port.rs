use ctx_history_index::{EventSearchCandidate, EventSearchFilters, VerifiedIndex};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticCapability {
    Available,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct HistorySemanticBatch {
    pub(crate) candidates: Vec<EventSearchCandidate>,
    pub(crate) diagnostics: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum HistorySemanticError {
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
    pub(crate) fn not_ready(
        code: &'static str,
        detail: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self::NotReady {
            code,
            detail: detail.into(),
            retryable,
        }
    }

    pub(crate) fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::NotReady { code, .. } => code,
            Self::Failed { .. } => "semantic_query_failed",
        }
    }

    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::NotReady { detail, .. } | Self::Failed { detail } => detail,
        }
    }

    #[cfg(test)]
    pub(crate) const fn retryable(&self) -> bool {
        match self {
            Self::NotReady { retryable, .. } => *retryable,
            Self::Failed { .. } => false,
        }
    }
}

pub(crate) trait HistorySemanticQuery {
    fn candidates(
        &mut self,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError>;
}

pub(crate) trait HistorySemanticPort: Send + Sync {
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
