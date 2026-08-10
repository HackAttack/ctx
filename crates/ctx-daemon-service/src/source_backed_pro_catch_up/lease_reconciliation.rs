use std::path::Path;

use anyhow::Result;

use crate::ProCatchUpPort;

use super::status::{
    persist_status, require_durable_status, SourceBackedProCatchUpError,
    SourceBackedProCatchUpState, SourceBackedProCatchUpStatus,
    SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION,
};

pub fn reconcile_core_finalization_generation_lease<P: ProCatchUpPort + ?Sized>(
    data_root: &Path,
    pro: &P,
) -> Result<()> {
    let lease = pro.finalization_lease(data_root)?;
    let Some(status) = require_durable_status(data_root)? else {
        if lease.is_some() {
            cancel_core_finalization_generation_lease(
                data_root,
                "stale Pro finalization lease had no durable job",
                pro,
            )?;
        }
        return Ok(());
    };
    if status.schema_version != SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION
        || status.owner != "daemon"
        || status.kind != "source_backed_pro_catch_up"
    {
        if lease.is_some() {
            cancel_core_finalization_generation_lease(
                data_root,
                "durable Pro finalization job identity was invalid",
                pro,
            )?;
            return Ok(());
        }
        anyhow::bail!(
            "invalid_response: durable source-backed Pro catch-up job identity is invalid"
        );
    }

    let terminal = !status.pending
        || status.status == SourceBackedProCatchUpState::Completed
        || (status.status == SourceBackedProCatchUpState::Error && !status.retryable);
    if terminal {
        if let Some(lease) = &lease {
            pro.release_observed_finalization_lease(data_root, lease)?;
        }
        return Ok(());
    }

    if let Some(progress) = &status.finalization_progress {
        let validation = (|| -> Result<()> {
            progress
                .validate()
                .map_err(|error| anyhow::anyhow!("invalid_response: {}", error.message))?;
            if progress.core_generation_id != status.core_generation_id {
                anyhow::bail!(
                    "invalid_response: durable Pro finalization progress targets a foreign Core generation"
                );
            }
            if lease.is_some() {
                pro.validate_finalization_lease(data_root, progress)
            } else {
                pro.reconstruct_finalization_lease(data_root, progress)
            }
        })();
        if let Err(error) = validation {
            if lease.is_some() {
                cancel_core_finalization_generation_lease(data_root, &error.to_string(), pro)?;
            } else {
                let failed = status
                    .clone()
                    .error(SourceBackedProCatchUpError::projection(error, pro));
                persist_status(data_root, &failed)?;
            }
        }
        return Ok(());
    }

    let Some(lease) = lease else {
        return Ok(());
    };
    if pro.finalization_lease_generation(&lease) != status.core_generation_id {
        cancel_core_finalization_generation_lease(
            data_root,
            "durable Core generation lease targeted a foreign Pro job",
            pro,
        )?;
        return Ok(());
    }
    if status.pending && status.status != SourceBackedProCatchUpState::Completed {
        // Finish can commit while its response is lost. Until helper status
        // supplies the finalization tuple, the generation-bound pending job is
        // the only durable identity available and the bounded lease must remain.
        return Ok(());
    }
    pro.release_finalization_lease(data_root, Some(&status.core_generation_id))?;
    Ok(())
}

pub fn cancel_core_finalization_generation_lease<P: ProCatchUpPort + ?Sized>(
    data_root: &Path,
    reason: &str,
    pro: &P,
) -> Result<bool> {
    let Some(lease) = pro.finalization_lease(data_root)? else {
        return Ok(false);
    };
    let attempts = require_durable_status(data_root)?
        .as_ref()
        .filter(|status| status.core_generation_id == pro.finalization_lease_generation(&lease))
        .map(|status| status.attempts)
        .unwrap_or(1);
    let cancelled =
        SourceBackedProCatchUpStatus::pending(pro.finalization_lease_generation(&lease), attempts)
            .cancelled(reason);
    persist_status(data_root, &cancelled)?;
    pro.release_observed_finalization_lease(data_root, &lease)
}
