use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::GROK_BUILD_SOURCE_FORMAT;

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v5";

pub(crate) const fn grok_build_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::GrokBuild,
        GROK_BUILD_SOURCE_FORMAT,
        "grok-build-acp-updates-jsonl-v1",
        PARSER_REVISION,
    )
}

pub(crate) fn grok_build_file_is_selected(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("updates.jsonl")
}

#[path = "grok_build_records.rs"]
mod records;
pub(super) use records::{
    enumerate_grok_build_results, grok_build_event_identity, grok_build_event_text,
    grok_build_event_type, grok_build_header_session_id, grok_build_role,
    grok_build_structured_tool_call_text, grok_build_timestamp,
};
