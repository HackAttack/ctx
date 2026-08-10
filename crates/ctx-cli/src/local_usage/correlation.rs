use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
    time::Duration,
};

use serde_json::Value;

use super::{
    mcp_search_context_targets, resolved_mcp_context_target, store, LocalUsageStorageAuthority,
    McpContextTarget, McpInvocation, Outcome, UsageControlRevision,
};

pub(super) const CONTEXT_CORRELATION_MAX_RECORDS: usize = 1_024;

#[derive(Debug, Clone, Copy, Default)]
struct ContextRecordState {
    opened: bool,
}

/// Bounded, process-local search-to-open correlation.
///
/// The keys never cross the persistence boundary. Definition 2 has no open or
/// citation-credit counter, so this state is observational only and is cleared
/// on a local-usage control revision. Citation correlation is intentionally
/// unsupported until a production citation event exists.
#[derive(Debug, Clone)]
struct EphemeralContextCorrelation<K> {
    records: HashMap<K, ContextRecordState>,
}

impl<K> Default for EphemeralContextCorrelation<K> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> EphemeralContextCorrelation<K> {
    fn record_found(&mut self, key: K) -> bool {
        if self.records.len() >= CONTEXT_CORRELATION_MAX_RECORDS {
            return false;
        }
        if let Entry::Vacant(entry) = self.records.entry(key) {
            entry.insert(ContextRecordState::default());
            true
        } else {
            false
        }
    }

    fn record_opened(&mut self, key: &K) -> bool {
        let Some(state) = self.records.get_mut(key) else {
            return false;
        };
        if state.opened {
            false
        } else {
            state.opened = true;
            true
        }
    }
}

pub(crate) struct McpUsageRecorder {
    storage: LocalUsageStorageAuthority,
    control: crate::observability_composition::LocalUsageControlAuthority,
    enabled: bool,
    control_revision: Option<UsageControlRevision>,
    context: EphemeralContextCorrelation<McpContextTarget>,
    #[cfg(test)]
    trace: Option<std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>>,
}

impl McpUsageRecorder {
    pub(crate) fn start_authorized(
        storage: LocalUsageStorageAuthority,
        mut control: crate::observability_composition::LocalUsageControlAuthority,
    ) -> Self {
        let snapshot = control.snapshot();
        Self {
            storage,
            control,
            enabled: snapshot.enabled(),
            control_revision: snapshot.revision().cloned(),
            context: EphemeralContextCorrelation::default(),
            #[cfg(test)]
            trace: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn start(data_root: std::path::PathBuf) -> Self {
        Self::start_authorized(
            crate::observability_composition::local_usage_storage_authority(&data_root),
            crate::observability_composition::LocalUsageControlAuthority::new(data_root),
        )
    }

    pub(crate) fn record_delivered(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        duration: Duration,
        serialized_response_bytes: usize,
    ) {
        self.refresh_control();
        if !self.enabled {
            return;
        }
        let operation = invocation.completed(response, duration, serialized_response_bytes);
        let mut next_context = self.context.clone();
        if operation.outcome == Outcome::Success {
            Self::apply_delivered_correlation(&mut next_context, &invocation, response);
        }
        if store::record_authorized(&self.storage, operation).is_ok() {
            self.context = next_context;
            #[cfg(test)]
            if let Some(trace) = &self.trace {
                trace.lock().unwrap().push("local_usage");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_test_trace(
        &mut self,
        trace: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    ) {
        self.trace = Some(trace);
    }

    fn refresh_control(&mut self) {
        let snapshot = self.control.snapshot();
        if !snapshot.available()
            || snapshot.revision().is_none()
            || self.control_revision.as_ref() != snapshot.revision()
            || self.enabled && !snapshot.enabled()
        {
            self.context = EphemeralContextCorrelation::default();
        }
        self.control_revision = snapshot.revision().cloned();
        self.enabled = snapshot.enabled();
    }

    fn apply_delivered_correlation(
        context: &mut EphemeralContextCorrelation<McpContextTarget>,
        invocation: &McpInvocation,
        response: &Value,
    ) -> bool {
        if invocation.operation == crate::operation_descriptor::LocalUsageOperation::Search {
            for target in mcp_search_context_targets(response) {
                context.record_found(target);
            }
            return false;
        }
        // Never correlate on the caller-supplied selector: show accepts UUID
        // prefixes. Only the canonical full ID returned by the successful
        // result is eligible. Missing canonical IDs make correlation
        // unavailable for that delivery.
        resolved_mcp_context_target(invocation.operation, response)
            .is_some_and(|target| context.record_opened(&target))
    }

    #[cfg(test)]
    pub(in crate::local_usage) fn correlate_delivered_for_test(
        &mut self,
        invocation: &McpInvocation,
        response: &Value,
    ) -> bool {
        Self::apply_delivered_correlation(&mut self.context, invocation, response)
    }
}
