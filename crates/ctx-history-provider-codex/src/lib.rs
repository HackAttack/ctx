//! Codex provider semantics, bound to capture only through provider-runtime.
//!
//! This crate owns Codex parsing, identity, attribution, bounded source
//! observation, and its JSONL adapters. The composing capture crate supplies
//! the lifecycle binding and route registration policy.

pub mod codex;

pub use ctx_history_provider_runtime::{CaptureError, Result};

pub const MAX_PROVIDER_JSONL_LINE_BYTES: usize =
    ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
pub const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;
pub const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl";

pub mod common {
    pub mod io {
        pub use ctx_history_provider_runtime::source_io::*;
    }

    pub mod json {
        pub use ctx_history_capture_model::{
            exact_bounded_string_alias, raw_object_keys_are_unique, ExactJsonStringAlias,
        };
    }
}

/// Compatibility namespace for the moved provider body. It intentionally
/// exposes only provider-runtime's generic JSONL aliases, never capture.
pub mod provider {
    pub use crate::codex;

    pub mod source_backed {
        pub use ctx_history_capture_runtime::BaseEventLookup;
        pub use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};
        pub use ctx_history_provider_runtime::{ProviderBaseEventLookup, ProviderRuntimeBinding};

        pub mod family {
            pub mod jsonl {
                pub use ctx_history_provider_runtime::*;

                pub type JsonlFamilyRuntime<B> = ProviderJsonlRuntime<B>;
                pub type JsonlReader = ProviderJsonlReader;
                pub type JsonlFamilyExecutionIo<B> = ProviderJsonlExecutionIo<B>;
                pub type JsonlFamilyInventory = ProviderJsonlInventory;
                pub type JsonlFamilyLeaf = ProviderJsonlLeaf;
                pub type JsonlFamilyOpenedMember<'a> = ProviderJsonlOpenedMember<'a>;
                pub type JsonlFamilyMembershipObservation = ProviderJsonlMembershipObservation;
                pub type JsonlFamilyWorkerContext<B> = ProviderJsonlWorkerContext<B>;
            }
        }
    }
}
