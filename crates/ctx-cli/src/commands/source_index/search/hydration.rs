pub use ctx_history_query::{presentations_for_search_hits, SearchPresentation};
#[cfg(test)]
pub use ctx_history_query::{
    presentations_for_search_hits_with_budget, SearchPresentationHydrationBudget,
    SearchPresentationRetentionBudgetExceeded, SEARCH_PRESENTATION_HYDRATION_BUDGET,
    SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
};
