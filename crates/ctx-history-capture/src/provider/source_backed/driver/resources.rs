use std::ops::Deref;

use ctx_history_capture_runtime::{
    CoreRouteByteLease, CoreRouteResourceError, CoreRouteResourceKind, CoreRouteResources,
};

use super::SourceBackedReconciliationDemand;

/// Capture-local route context layered over the provider-neutral byte budget.
///
/// Hermes chooses incremental versus exhaustive reconciliation at the capture
/// boundary. The neutral runtime continues to own the exact shared byte and
/// worker budgets without importing that provider policy.
#[derive(Debug, Clone)]
pub(crate) struct SourceBackedRouteResources {
    core: CoreRouteResources,
    reconciliation_demand: SourceBackedReconciliationDemand,
}

impl SourceBackedRouteResources {
    pub(crate) fn production(leaf_worker_budget: usize) -> Self {
        Self {
            core: CoreRouteResources::production(leaf_worker_budget),
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
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
        }
    }

    pub(crate) fn with_reconciliation_demand(
        mut self,
        demand: SourceBackedReconciliationDemand,
    ) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    pub(crate) const fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.reconciliation_demand
    }

    pub(crate) fn leaf_worker_budget(&self) -> usize {
        self.core.leaf_worker_budget()
    }

    pub(crate) fn maximum_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.maximum_bytes(kind)
    }

    pub(crate) fn core_output_batch_reservation_bytes(&self) -> u64 {
        self.core.core_output_batch_reservation_bytes()
    }

    pub(crate) fn reserve(
        &self,
        kind: CoreRouteResourceKind,
        bytes: usize,
    ) -> Result<CoreRouteByteLease, CoreRouteResourceError> {
        self.core.reserve(kind, bytes)
    }

    #[cfg(test)]
    pub(crate) fn live_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        self.core.live_bytes(kind)
    }
}

impl Deref for SourceBackedRouteResources {
    type Target = CoreRouteResources;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
