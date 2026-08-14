//! Compatibility façade for the extracted native JSONL provider package.

use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::Result;

const _: Option<super::super::source_backed::FallbackEventIdentityState> = None;

pub(crate) fn visit_native_jsonl_files(
    root: &Path,
    provider: CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    ctx_history_provider_native_jsonl::visit_native_jsonl_files_with(root, provider, visit)
}
