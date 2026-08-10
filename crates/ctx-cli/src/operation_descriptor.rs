use crate::analytics::{
    DaemonOperationV1, DocsTelemetry, DoctorTelemetry, ImportTelemetry, IndexTelemetry,
    IntegrationTelemetry, LocateTelemetry, McpErrorClassV1, McpErrorLayerV1, McpMethodV1,
    McpResultMetadataV1, ProHostOperationV1, SearchTelemetry, SetupTelemetry, ShowTelemetry,
    SourcesTelemetry, StatusTelemetry, UpgradeTelemetry,
};

/// Closed, transport-neutral identity and telemetry facts for one product operation.
///
/// CLI and MCP adapters classify their transport input once, then analytics and
/// aggregate-only local usage project this value without inspecting Clap values,
/// JSON-RPC payloads, or open-ended operation strings.
#[derive(Debug)]
pub(crate) enum OperationDescriptor {
    Cli(CliOperation),
    Mcp(McpOperation),
    ProHost(ProHostOperationV1),
    Daemon(DaemonOperationV1),
}

#[derive(Debug)]
pub(crate) enum CliOperation {
    Setup(SetupTelemetry),
    Status(StatusTelemetry),
    Stats,
    Index(IndexTelemetry),
    Sources(SourcesTelemetry),
    Import(ImportTelemetry),
    ShowSession(ShowTelemetry),
    ShowEvent(ShowTelemetry),
    Locate(LocateTelemetry),
    Search(SearchTelemetry),
    ProSetup,
    ProManage,
    ProUninstall,
    Referral,
    Blame,
    Docs(DocsTelemetry),
    Integrations(IntegrationTelemetry),
    McpServe,
    DaemonRun,
    DaemonStatus,
    DaemonEnable,
    DaemonDisable,
    Upgrade {
        telemetry: UpgradeTelemetry,
        record_local_usage: bool,
    },
    Doctor(DoctorTelemetry),
}

impl CliOperation {
    pub(crate) const fn analytics_name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Stats => "stats",
            Self::Index(_) => "index",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::ShowSession(_) | Self::ShowEvent(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::ProSetup => "pro_setup",
            Self::ProManage => "pro_manage",
            Self::ProUninstall => "pro_uninstall",
            Self::Referral => "referral",
            Self::Blame => "blame",
            Self::Docs(_) => "docs",
            Self::Integrations(_) => "integration",
            Self::McpServe => "serve",
            Self::DaemonRun => "run",
            Self::DaemonStatus => "status",
            Self::DaemonEnable => "enable",
            Self::DaemonDisable => "disable",
            Self::Upgrade { .. } => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }

    pub(crate) const fn emits_client_analytics(&self) -> bool {
        matches!(
            self,
            Self::Setup(_)
                | Self::Status(_)
                | Self::Index(_)
                | Self::Sources(_)
                | Self::Import(_)
                | Self::ShowSession(_)
                | Self::ShowEvent(_)
                | Self::Locate(_)
                | Self::Search(_)
                | Self::Docs(_)
                | Self::Integrations(_)
                | Self::Upgrade { .. }
                | Self::Doctor(_)
        )
    }

    pub(crate) const fn local_usage_operation(&self) -> Option<LocalUsageOperation> {
        match self {
            Self::Setup(_) => Some(LocalUsageOperation::Setup),
            Self::Status(_) | Self::Stats => None,
            Self::Index(_) => Some(LocalUsageOperation::Index),
            Self::Sources(_) => Some(LocalUsageOperation::Sources),
            Self::Import(_) => Some(LocalUsageOperation::Import),
            Self::ShowSession(_) => Some(LocalUsageOperation::ShowSession),
            Self::ShowEvent(_) => Some(LocalUsageOperation::ShowEvent),
            Self::Locate(_) => Some(LocalUsageOperation::Locate),
            Self::Search(_) => Some(LocalUsageOperation::Search),
            Self::ProSetup => Some(LocalUsageOperation::ProSetup),
            Self::ProManage => Some(LocalUsageOperation::ProManage),
            Self::ProUninstall => Some(LocalUsageOperation::ProUninstall),
            Self::Referral => None,
            Self::Blame => Some(LocalUsageOperation::Blame),
            Self::Docs(_) => Some(LocalUsageOperation::Docs),
            Self::Integrations(_) => Some(LocalUsageOperation::Integrations),
            Self::McpServe | Self::DaemonRun => None,
            Self::DaemonStatus => Some(LocalUsageOperation::DaemonStatus),
            Self::DaemonEnable => Some(LocalUsageOperation::DaemonEnable),
            Self::DaemonDisable => Some(LocalUsageOperation::DaemonDisable),
            Self::Upgrade {
                record_local_usage: true,
                ..
            } => Some(LocalUsageOperation::Upgrade),
            Self::Upgrade {
                record_local_usage: false,
                ..
            } => None,
            Self::Doctor(_) => Some(LocalUsageOperation::Doctor),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpOperationKind {
    Status,
    Sources,
    Search,
    ShowSession,
    ShowEvent,
    QueryEvents,
    Blame,
    ProStatus,
    Unknown,
    Missing,
}

impl McpOperationKind {
    /// Sole production authority for mapping an MCP tool name to product identity.
    pub(crate) fn from_tool_name(name: Option<&str>) -> Self {
        match name {
            Some("status") => Self::Status,
            Some("sources") => Self::Sources,
            Some("search") => Self::Search,
            Some("show_session") => Self::ShowSession,
            Some("show_event") => Self::ShowEvent,
            Some("query_events") => Self::QueryEvents,
            Some("blame") => Self::Blame,
            Some("pro_status") => Self::ProStatus,
            Some(_) => Self::Unknown,
            None => Self::Missing,
        }
    }

    pub(crate) const fn tool_name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Sources => "sources",
            Self::Search => "search",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::QueryEvents => "query_events",
            Self::Blame => "blame",
            Self::ProStatus => "pro_status",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }

    pub(crate) const fn local_usage_operation(self) -> Option<LocalUsageOperation> {
        match self {
            Self::Status => Some(LocalUsageOperation::Status),
            Self::Sources => Some(LocalUsageOperation::Sources),
            Self::Search => Some(LocalUsageOperation::Search),
            Self::ShowSession => Some(LocalUsageOperation::ShowSession),
            Self::ShowEvent | Self::QueryEvents => Some(LocalUsageOperation::ShowEvent),
            Self::Blame => Some(LocalUsageOperation::Blame),
            Self::ProStatus => Some(LocalUsageOperation::ProStatus),
            Self::Unknown | Self::Missing => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct McpOperation {
    method: McpMethodV1,
    kind: McpOperationKind,
    error_layer: Option<McpErrorLayerV1>,
    error_class: Option<McpErrorClassV1>,
    result: McpResultMetadataV1,
}

impl McpOperation {
    const fn new(method: McpMethodV1, kind: McpOperationKind) -> Self {
        Self {
            method,
            kind,
            error_layer: None,
            error_class: None,
            result: McpResultMetadataV1 {
                result_count: None,
                zero_result: None,
                result_truncated: None,
                events_truncated: None,
                response_bound: None,
            },
        }
    }

    pub(crate) const fn tool_call(kind: McpOperationKind) -> Self {
        Self::new(McpMethodV1::ToolsCall, kind)
    }

    pub(crate) const fn unknown_request() -> Self {
        Self::new(McpMethodV1::Unknown, McpOperationKind::Missing)
    }

    pub(crate) const fn missing_request() -> Self {
        Self::new(McpMethodV1::Missing, McpOperationKind::Missing)
    }

    pub(crate) const fn method(self) -> McpMethodV1 {
        self.method
    }

    pub(crate) const fn kind(self) -> McpOperationKind {
        self.kind
    }

    pub(crate) const fn error_layer(self) -> Option<McpErrorLayerV1> {
        self.error_layer
    }

    pub(crate) const fn error_class(self) -> Option<McpErrorClassV1> {
        self.error_class
    }

    pub(crate) const fn result(self) -> McpResultMetadataV1 {
        self.result
    }

    pub(crate) fn with_error(mut self, layer: McpErrorLayerV1, class: McpErrorClassV1) -> Self {
        self.error_layer = Some(layer);
        self.error_class = Some(class);
        self
    }

    pub(crate) fn with_result(mut self, result: McpResultMetadataV1) -> Self {
        self.result = result;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalUsageOperation {
    Setup,
    Status,
    Index,
    Sources,
    Import,
    ShowSession,
    ShowEvent,
    Locate,
    Search,
    ProSetup,
    ProManage,
    ProUninstall,
    ProStatus,
    Blame,
    Docs,
    Integrations,
    DaemonStatus,
    DaemonEnable,
    DaemonDisable,
    Upgrade,
    Doctor,
}

impl LocalUsageOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Status => "status",
            Self::Index => "index",
            Self::Sources => "sources",
            Self::Import => "import",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::Locate => "locate",
            Self::Search => "search",
            Self::ProSetup => "pro_setup",
            Self::ProManage => "pro_manage",
            Self::ProUninstall => "pro_uninstall",
            Self::ProStatus => "pro_status",
            Self::Blame => "blame",
            Self::Docs => "docs",
            Self::Integrations => "integrations",
            Self::DaemonStatus => "daemon_status",
            Self::DaemonEnable => "daemon_enable",
            Self::DaemonDisable => "daemon_disable",
            Self::Upgrade => "upgrade",
            Self::Doctor => "doctor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultObservationAction {
    Search,
    OpenSession,
    OpenEvent,
    Locate,
    Sources,
    Blame,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_identity_is_closed_and_round_trips() {
        for (name, kind) in [
            ("status", McpOperationKind::Status),
            ("sources", McpOperationKind::Sources),
            ("search", McpOperationKind::Search),
            ("show_session", McpOperationKind::ShowSession),
            ("show_event", McpOperationKind::ShowEvent),
            ("query_events", McpOperationKind::QueryEvents),
            ("blame", McpOperationKind::Blame),
            ("pro_status", McpOperationKind::ProStatus),
        ] {
            assert_eq!(McpOperationKind::from_tool_name(Some(name)), kind);
            assert_eq!(kind.tool_name(), name);
        }
        assert_eq!(
            McpOperationKind::from_tool_name(Some("private input")),
            McpOperationKind::Unknown
        );
        assert_eq!(
            McpOperationKind::from_tool_name(None),
            McpOperationKind::Missing
        );
    }

    #[test]
    fn local_usage_projection_preserves_surface_specific_names() {
        assert_eq!(
            McpOperationKind::QueryEvents.local_usage_operation(),
            Some(LocalUsageOperation::ShowEvent)
        );
        assert_eq!(LocalUsageOperation::Integrations.as_str(), "integrations");
        assert_eq!(McpOperationKind::Unknown.local_usage_operation(), None);
    }
}
