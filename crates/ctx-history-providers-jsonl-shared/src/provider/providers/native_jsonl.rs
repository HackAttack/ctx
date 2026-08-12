use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_source_io::visit_bounded_tree_files;

use crate::Result;

/// Visits the JSONL leaves used by the providers in this pack. Their source
/// layouts accept every regular `.jsonl` file under the selected root.
pub(crate) fn visit_native_jsonl_files(
    root: &Path,
    _provider: CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    visit_bounded_tree_files(
        root,
        &mut |candidate| {
            candidate
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("jsonl")
        },
        &mut |source_file| visit(source_file.path()),
    )
}
