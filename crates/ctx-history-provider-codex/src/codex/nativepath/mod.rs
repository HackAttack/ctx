//! Provider-owned Codex source discovery, parsing, and direct Core indexing.

mod checkpoint;
pub mod prompt_history;
mod reader;
mod record;
mod rows;
mod source;
pub mod source_backed;

pub(crate) use checkpoint::MAX_CODEX_TOOL_CONTEXTS;
pub use prompt_history::{
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistoryProjector,
    CodexPromptHistorySourceBackedInputV0,
};
pub(crate) use reader::{opened_codex_file_observation, CodexNativeScanner};
#[cfg(test)]
pub(crate) use reader::{
    CodexScanCounters, MAX_CODEX_PAGE_BYTES, MAX_CODEX_PAGE_ROWS, MAX_CODEX_RECORD_BYTES,
    MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES,
};
pub(crate) use rows::CodexSessionRow;
#[cfg(test)]
pub(crate) use source::CodexCatalogSource;
pub(crate) use source::{discover_codex_catalog_sources, CodexFileObservation};
pub use source_backed::{
    absolute_lexical_path, codex_session_root_rank, CodexExplicitSessionSourceBackedInputV0,
    CodexGenerationNormalizationCoordinatorV0, CodexSessionJsonlFamilyAdapterV0,
};
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
#[cfg(any(test, feature = "test-support"))]
pub use source_backed::{
    install_after_codex_causal_stage_hook_v1, install_after_codex_metadata_inventory_hook,
    CodexCausalSourceObservationV1,
};
