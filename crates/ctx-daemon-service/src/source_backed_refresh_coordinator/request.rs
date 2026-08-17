use super::*;

/// The caller's requested Core operation, independent of how it observes the
/// daemon attempt and whether it supplied an explicit catalog snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshOperation {
    Refresh,
    Import,
}

impl SourceBackedRefreshOperation {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Import => "import",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshTrigger {
    Setup,
    Search,
    Import,
}

impl SourceBackedRefreshTrigger {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Search => "search",
            Self::Import => "import",
        }
    }

    pub(super) const fn daemon_trigger(self) -> crate::DaemonTrigger {
        match self {
            Self::Setup => crate::DaemonTrigger::Setup,
            Self::Search => crate::DaemonTrigger::Search,
            Self::Import => crate::DaemonTrigger::Import,
        }
    }
}

/// Typed source-refresh IPC request. Exact imports carry their one-shot
/// request overlay inline; automatic refreshes carry no catalog authority.
pub(super) struct SourceBackedRefreshRequest<'a> {
    request_id: String,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    selector: SourceBackedRefreshSelector,
    explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
    trigger: SourceBackedRefreshTrigger,
}

impl<'a> SourceBackedRefreshRequest<'a> {
    pub(super) fn new(
        mode: SourceBackedRefreshMode,
        operation: SourceBackedRefreshOperation,
        selector: SourceBackedRefreshSelector,
        explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
        fresh_after_admitted_snapshot: bool,
    ) -> Self {
        Self {
            request_id: Uuid::now_v7().to_string(),
            mode,
            operation,
            selector,
            explicit_source_catalog,
            fresh_after_admitted_snapshot,
            trigger: match operation {
                SourceBackedRefreshOperation::Refresh => SourceBackedRefreshTrigger::Search,
                SourceBackedRefreshOperation::Import => SourceBackedRefreshTrigger::Import,
            },
        }
    }

    pub(super) fn with_trigger(mut self, trigger: SourceBackedRefreshTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    pub(super) fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = request_id.into();
        self
    }

    pub(super) fn to_json(&self) -> Result<Value> {
        Ok(compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": self.request_id,
            "mode": self.mode.as_str(),
            "operation": self.operation.as_str(),
            "trigger": self.trigger.as_str(),
            "refresh_selector": self.selector.to_json(),
            "explicit_source_catalog": self.explicit_source_catalog
                .map(ExplicitSourceCatalogAuthority::to_json),
            "fresh_after_admitted_snapshot": self.fresh_after_admitted_snapshot,
        })))
    }
}
