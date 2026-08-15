use std::{
    collections::BTreeSet,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    CoreRouteByteLease, CoreRouteResourceError, CoreRouteResourceKind, CoreRouteResources,
};

use super::SourceBackedReconciliationDemand;

pub type SourceBackedRouteResourceKind = CoreRouteResourceKind;
pub type SourceBackedRouteByteReservation = CoreRouteByteLease;

/// Source-backed route context layered over the provider-neutral byte budget.
///
/// Hermes chooses incremental versus exhaustive reconciliation at the capture
/// boundary. The neutral runtime continues to own the exact shared byte and
/// worker budgets without importing that provider policy.
#[derive(Debug, Clone)]
pub struct SourceBackedRouteResources {
    core: CoreRouteResources,
    reconciliation_demand: SourceBackedReconciliationDemand,
    member_workset: Option<Arc<BTreeSet<PathBuf>>>,
}

impl SourceBackedRouteResources {
    pub fn production(leaf_worker_budget: usize) -> Self {
        Self {
            core: CoreRouteResources::production(leaf_worker_budget),
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
            member_workset: None,
        }
    }

    #[doc(hidden)]
    pub fn for_test(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self {
            core: CoreRouteResources::for_test(
                leaf_worker_budget,
                maximum_live_output_bytes,
                maximum_physical_scratch_bytes,
            ),
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
            member_workset: None,
        }
    }

    pub fn with_reconciliation_demand(mut self, demand: SourceBackedReconciliationDemand) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    pub const fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.reconciliation_demand
    }

    pub fn with_member_workset(mut self, members: Option<BTreeSet<PathBuf>>) -> Self {
        self.member_workset = members.map(Arc::new);
        self
    }

    pub fn member_workset(&self) -> Option<&BTreeSet<PathBuf>> {
        self.member_workset.as_deref()
    }

    pub fn member_selected(&self, path: &Path) -> bool {
        self.member_workset()
            .is_some_and(|members| members.contains(path))
    }

    pub fn leaf_worker_budget(&self) -> usize {
        self.core.leaf_worker_budget()
    }

    pub fn maximum_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.maximum_bytes(kind)
    }

    pub fn core_output_batch_reservation_bytes(&self) -> u64 {
        self.core.core_output_batch_reservation_bytes()
    }

    pub fn reserve(
        &self,
        kind: CoreRouteResourceKind,
        bytes: usize,
    ) -> Result<CoreRouteByteLease, CoreRouteResourceError> {
        self.core.reserve(kind, bytes)
    }

    #[doc(hidden)]
    pub fn live_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.live_bytes(kind)
    }
}

impl Deref for SourceBackedRouteResources {
    type Target = CoreRouteResources;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
