use std::time::Duration;

use ctx_history_index::VerifiedIndex;
use serde_json::Value;

use super::{
    paths_status::{daemon_jobs_path, write_daemon_job_status},
    source_backed_refresh_coordinator::{PinnedCorePublication, PinnedSourceBackedGeneration},
};

mod finalization;
mod lease_reconciliation;
mod recheck;
mod status;

pub(super) use finalization::run_after_core_publication;
pub use finalization::wait_for_completed_generation;
pub use lease_reconciliation::cancel_core_finalization_generation_lease;
pub(super) use lease_reconciliation::reconcile_core_finalization_generation_lease;
pub(super) use recheck::schedule as helper_recheck_schedule;
pub use recheck::{
    publish as publish_helper_recheck_intent, targets as helper_recheck_targets,
    wake as wake_helper_recheck,
};
pub(super) use status::{
    persist_status_json, read_status_json, scheduled_target_generation, status_generation,
    status_has_finalization_pending,
};

const SOURCE_BACKED_PRO_CATCH_UP_WAKE_TIMEOUT: Duration = Duration::from_millis(500);
const SOURCE_BACKED_PRO_CATCH_UP_WAKE_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

pub(super) struct SourceBackedProCatchUpRun {
    pub(super) status: Value,
    pub(super) did_work: bool,
    pub(super) continuation_pending: bool,
}

#[derive(Clone, Copy)]
pub(super) enum SourceBackedProCoreAuthority<'a> {
    Retained(&'a PinnedCorePublication),
    Durable(&'a PinnedSourceBackedGeneration),
}

impl<'a> SourceBackedProCoreAuthority<'a> {
    pub(super) fn generation_id(self) -> &'a str {
        match self {
            Self::Retained(authority) => authority.generation_id(),
            Self::Durable(authority) => authority.generation_id(),
        }
    }

    fn verified_index(self) -> &'a VerifiedIndex {
        match self {
            Self::Retained(authority) => authority.verified_index_ref(),
            Self::Durable(authority) => authority.verified_index(),
        }
    }

    fn surface(self) -> &'static str {
        match self {
            Self::Retained(_) => "retained Core generation pin",
            Self::Durable(_) => "durable active Core generation pin",
        }
    }
}
