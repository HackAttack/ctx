use std::path::Path;

use anyhow::{bail, Context, Result};
use ctx_history_refresh::RefreshSelection;

use crate::{
    progress::ProgressReporter,
    semantic::{
        coordinate_import_source_backed_refresh_with_progress, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
    },
};

/// Applies import-specific policy around the one Core refresh control path.
///
/// Import may start the daemon and waits only for authoritative Core publication.
pub(super) fn wait_for_import_core_refresh(
    data_root: &Path,
    no_daemon: bool,
    selection: RefreshSelection,
    progress: &mut ProgressReporter<'_>,
) -> Result<SourceBackedRefreshObservation> {
    let mut report_progress = |update: &crate::semantic::RefreshStatus| {
        progress.source_refresh(update).map_err(anyhow::Error::new)
    };
    let refresh = coordinate_import_source_backed_refresh_with_progress(
        data_root,
        SourceBackedRefreshMode::Wait,
        selection,
        !no_daemon,
        &mut report_progress,
    )
    .context("publish provider inputs through the Core refresh engine")?;

    let receipt = refresh
        .receipt
        .as_ref()
        .context("Core refresh completed without an authoritative publication receipt")?;
    if refresh.pin.generation_id() != receipt.published_generation {
        bail!(
            "Core refresh receipt names generation {}, but the verified publication pin carries {}",
            receipt.published_generation,
            refresh.pin.generation_id()
        );
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    #[test]
    fn import_control_contains_no_ingestion_provider_read_or_sidecar_implementation() {
        let source = include_str!("core_refresh.rs");
        for forbidden in [
            ["ctx_history_", "capture"].concat(),
            ["ImportCore", "RefreshRequest"].concat(),
            ["SourceBackedRefresh", "Selector"].concat(),
            ["SourceBackedRefresh", "Executor"].concat(),
            ["VerifiedIndex", "::open"].concat(),
            ["Store", "::open"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "import Core control contains forbidden foreground implementation `{forbidden}`"
            );
        }
    }
}
