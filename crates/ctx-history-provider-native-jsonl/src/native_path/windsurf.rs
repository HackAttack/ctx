use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::{NativeJsonlRuntime, WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT};

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v6-optional-activity-admission";

pub(crate) use ctx_history_native_jsonl_parsers::windsurf::{
    event_role as windsurf_event_role, event_text as windsurf_event_text,
    event_type as windsurf_event_type,
};

pub(crate) fn windsurf_session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_owned)
}

pub const fn windsurf_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Windsurf,
        WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
        "windsurf-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}
