//! Claude Code and Cursor provider adapters for ctx agent history.
//!
//! This pack owns provider discovery, parsing, identity, checkpointing, and
//! projection. Capture supplies the concrete lifecycle binding and retains
//! registration, index lifecycle, and publication authority.

mod claude;
pub mod cursor;
#[cfg(test)]
#[path = "tests/runtime.rs"]
pub(crate) mod test_runtime;

use std::sync::Arc;

use ctx_history_jsonl::JsonlFamilyAdapter;
use ctx_history_provider_runtime::{ProviderJsonlRuntime, ProviderRuntimeBinding};

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub mod cursor {
        use super::super::cursor::source_backed::source_key;
        use super::super::cursor::source_backed::{
            cursor_base_identity_probes, cursor_signature_records,
            reset_cursor_base_identity_probes, reset_cursor_projected_records,
            reset_cursor_signature_records, take_cursor_projected_records,
        };

        pub fn reset_projected_records(native_session_id: &str) {
            let source = source_key(native_session_id).expect("valid Cursor test session id");
            reset_cursor_projected_records(&source);
        }

        pub fn take_projected_records(native_session_id: &str) -> u64 {
            let source = source_key(native_session_id).expect("valid Cursor test session id");
            take_cursor_projected_records(&source)
        }

        pub fn reset_signature_records() {
            reset_cursor_signature_records();
        }

        pub fn signature_records() -> u64 {
            cursor_signature_records()
        }

        pub fn reset_base_identity_probes() {
            reset_cursor_base_identity_probes();
        }

        pub fn base_identity_probes() -> u64 {
            cursor_base_identity_probes()
        }
    }
}

pub use cursor::{discover_cursor_transcripts, CursorDiscoveryIssueKind};

const CLAUDE_PROJECTS_SOURCE_FORMAT: &str = "claude_projects_jsonl_tree";
const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl_tree";

pub fn claude_jsonl_adapter<B>() -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    claude::nativepath::source_backed::claude_jsonl_adapter::<B>()
}

pub fn cursor_jsonl_adapter<B>() -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    cursor::cursor_jsonl_adapter::<B>()
}
