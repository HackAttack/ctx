#[cfg(test)]
pub(crate) use ctx_history_read_application::copied_lineage_read_model;
pub use ctx_history_read_application::{
    copied_lineage_relationship_summary, copied_lineage_summary,
};

#[cfg(test)]
pub(crate) fn copied_lineage_value(
    index: &ctx_history_index::VerifiedIndex,
    selected_event_id: uuid::Uuid,
    policy: ctx_history_index::CopiedEventLineagePolicy,
) -> anyhow::Result<serde_json::Value> {
    copied_lineage_read_model(&index.copied_event_lineage(selected_event_id, policy)?)
}
