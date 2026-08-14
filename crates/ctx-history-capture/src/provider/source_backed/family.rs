//! Shared physical source-family engines.
//!
//! Families own bounded source access and replay evidence. Provider adapters
//! retain every semantic decision, including parsing, identity, counters, and
//! projection.

pub(crate) mod document;
#[path = "family/jsonl_compat.rs"]
pub(crate) mod jsonl;

/// Capture-owned composition binding supplied to index-free provider packs.
#[derive(Clone, Copy)]
pub struct CaptureProviderRuntime;

#[doc(hidden)]
pub use document::CaptureDocumentSpool;

impl ctx_history_provider_runtime::ProviderRuntimeBinding for CaptureProviderRuntime {
    type CaptureLifecycleSink = super::IndexCaptureLifecycle;
    type DocumentRecordSpool = document::CaptureDocumentSpool;
}

#[cfg(test)]
#[path = "family/tests.rs"]
mod tests;
