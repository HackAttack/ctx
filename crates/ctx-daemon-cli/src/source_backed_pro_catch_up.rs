use std::path::Path;

use anyhow::Result;

pub fn cancel_core_finalization_generation_lease(data_root: &Path, reason: &str) -> Result<bool> {
    ctx_daemon_service::cancel_core_finalization_generation_lease(
        data_root,
        reason,
        &super::daemon_service_ports::PRO_CATCH_UP,
    )
}

#[cfg(test)]
mod tests;
