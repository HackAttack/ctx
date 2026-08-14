use std::sync::Arc;

use crate::provider::source_backed::family::jsonl::CaptureJsonlRuntime;

pub(crate) fn jsonl_adapter(
    source_format: &'static str,
) -> ctx_history_providers_jsonl_shared::Result<
    Arc<dyn ctx_history_jsonl::JsonlFamilyAdapter<Runtime = CaptureJsonlRuntime>>,
> {
    ctx_history_providers_jsonl_shared::adapters::deepseek_harness::<CaptureJsonlRuntime>(
        source_format,
    )
}
