use std::path::Path;

use anyhow::Result;

use crate::ProCatchUpPort;

pub fn reconcile_core_finalization_generation_lease(
    data_root: &Path,
    pro: &dyn ProCatchUpPort,
) -> Result<()> {
    pro.reconcile_finalization_lease(data_root)
}

pub fn cancel_core_finalization_generation_lease(
    data_root: &Path,
    reason: &str,
    pro: &dyn ProCatchUpPort,
) -> Result<bool> {
    pro.cancel_finalization_lease(data_root, reason)
}
