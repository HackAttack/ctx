mod discovery;
mod dto;
mod file_invocation;
mod parser;
mod source_backed;

pub(crate) use discovery::discover_gemini_transcripts;
pub(crate) use dto::{
    GeminiEventIdentity, GeminiFileObservation, GeminiRetainedEvent, GeminiScanError,
    GeminiSession, GeminiTranscriptSource,
};
#[doc(hidden)]
pub use parser::gemini_result_terminal_authority_is_ambiguous;
pub use source_backed::gemini_jsonl_adapter;

#[cfg(test)]
mod tests;
