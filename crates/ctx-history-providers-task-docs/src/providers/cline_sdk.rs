//! Current Cline SDK compound local-session store.
//!
//! This is intentionally separate from `task_json`: the legacy VS Code task
//! directory format remains a distinct import contract.

#[path = "cline_sdk/projection.rs"]
mod projection;
#[path = "cline_sdk/source.rs"]
mod source;
#[path = "cline_sdk/source_backed.rs"]
mod source_backed;

pub use source_backed::ClineSdkDocumentTreeAdapter;

#[cfg(test)]
#[path = "cline_sdk/tests.rs"]
mod tests;
