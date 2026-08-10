pub(crate) use ctx_client_observability::operation_descriptor::*;

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

    pub(crate) const fn observed(self) -> Option<ObservedMcpProductOperation> {
        match self {
            Self::Status => Some(ObservedMcpProductOperation::Status),
            Self::Sources => Some(ObservedMcpProductOperation::Sources),
            Self::Search => Some(ObservedMcpProductOperation::Search),
            Self::ShowSession => Some(ObservedMcpProductOperation::ShowSession),
            Self::ShowEvent => Some(ObservedMcpProductOperation::ShowEvent),
            Self::QueryEvents => Some(ObservedMcpProductOperation::QueryEvents),
            Self::Blame => Some(ObservedMcpProductOperation::Blame),
            Self::ProStatus => Some(ObservedMcpProductOperation::ProStatus),
            Self::Unknown | Self::Missing => None,
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
}
