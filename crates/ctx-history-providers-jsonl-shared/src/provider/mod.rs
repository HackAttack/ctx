pub(crate) mod custom_history_jsonl;
pub(crate) mod providers;

pub(crate) mod source_backed {
    pub(crate) use ctx_history_capture_runtime::{BaseEventLookup, SourceBackedRouteErrorKind};
    pub(crate) type IndexBaseEventLookup<R> = ctx_history_jsonl::JsonlRuntimeLookup<R>;
    pub(crate) type FallbackEventIdentityState<R> =
        ctx_history_jsonl::FallbackEventIdentityState<IndexBaseEventLookup<R>, crate::CaptureError>;
    pub(crate) mod family {
        pub(crate) mod jsonl {
            pub(crate) use crate::jsonl::*;
            pub(crate) type JsonlReader = ctx_history_jsonl::JsonlReader<crate::CaptureError>;
            pub(crate) type JsonlPhysicalStream =
                ctx_history_jsonl::JsonlPhysicalStream<crate::CaptureError>;
            pub(crate) type JsonlFamilyLeaf =
                ctx_history_jsonl::JsonlFamilyLeaf<crate::CaptureError>;
            pub(crate) type JsonlFamilyInventory =
                ctx_history_jsonl::JsonlFamilyInventory<crate::CaptureError>;
            pub(crate) type JsonlFamilyOptimizedLeafOutcome =
                ctx_history_jsonl::JsonlFamilyOptimizedLeafOutcome<crate::CaptureError>;
            pub(crate) type JsonlFamilyWorkerContext<R> =
                ctx_history_jsonl::JsonlFamilyWorkerContext<R>;
        }
    }
}

pub(crate) use ctx_history_source_io::provider_safe_path_segment;
