use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_source_io::visit_bounded_tree_files;

use crate::Result;

mod dialect;
pub(crate) mod native_path;
mod normalization;
pub(crate) mod result_content;

pub(crate) fn visit_native_jsonl_files(
    root: &Path,
    provider: CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    visit_bounded_tree_files(
        root,
        &mut |candidate| dialect::native_jsonl_file_candidate_is_selected(provider, candidate),
        &mut |source_file| visit(source_file.path()),
    )
}
#[cfg(test)]
mod tests;
