use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeScope};

use super::DEFAULT_EVENT_QUERY_LIMIT;
use crate::{
    ListEventsContentProjection, ListEventsDirection, ListEventsRequest, ListEventsScope,
    OutputFormat,
};
pub use ctx_history_read_application::{EventContentProjection, EventQueryWireRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEventsArgs {
    pub since: Option<String>,
    pub until: Option<String>,
    pub provider: Vec<String>,
    pub source: Option<String>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session: Option<String>,
    pub session: Option<String>,
    pub parent_session: Option<String>,
    pub root_session: Option<String>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_type: Option<String>,
    pub scope: EventQueryScope,
    pub file: Option<String>,
    pub direction: EventQueryDirection,
    pub cursor: Option<String>,
    pub limit: u64,
    pub content: EventContentProjectionArg,
    pub format: EventQueryFormat,
}

impl Default for ListEventsArgs {
    fn default() -> Self {
        Self {
            since: None,
            until: None,
            provider: Vec::new(),
            source: None,
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            provider_session: None,
            session: None,
            parent_session: None,
            root_session: None,
            branch: None,
            workspace: None,
            event_type: None,
            role: None,
            agent_type: None,
            scope: EventQueryScope::All,
            file: None,
            direction: EventQueryDirection::Ascending,
            cursor: None,
            limit: DEFAULT_EVENT_QUERY_LIMIT,
            content: EventContentProjectionArg::Full,
            format: EventQueryFormat::Json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventQueryFormat {
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventContentProjectionArg {
    Full,
    Text,
    None,
}
impl From<EventContentProjectionArg> for EventContentProjection {
    fn from(value: EventContentProjectionArg) -> Self {
        match value {
            EventContentProjectionArg::Full => Self::Full,
            EventContentProjectionArg::Text => Self::Text,
            EventContentProjectionArg::None => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventQueryScope {
    All,
    Primary,
    Subagent,
}
impl From<EventQueryScope> for CoreEventRangeScope {
    fn from(value: EventQueryScope) -> Self {
        match value {
            EventQueryScope::All => Self::All,
            EventQueryScope::Primary => Self::Primary,
            EventQueryScope::Subagent => Self::Subagent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventQueryDirection {
    Ascending,
    Descending,
}
impl From<EventQueryDirection> for CoreEventRangeDirection {
    fn from(value: EventQueryDirection) -> Self {
        match value {
            EventQueryDirection::Ascending => Self::Ascending,
            EventQueryDirection::Descending => Self::Descending,
        }
    }
}

impl From<ListEventsArgs> for ListEventsRequest {
    fn from(args: ListEventsArgs) -> Self {
        Self {
            since: args.since,
            until: args.until,
            providers: args.provider,
            source: args.source,
            history_source: args.history_source,
            provider_key: args.provider_key,
            source_id: args.source_id,
            source_format: args.source_format,
            provider_session: args.provider_session,
            session: args.session,
            parent_session: args.parent_session,
            root_session: args.root_session,
            branch: args.branch,
            workspace: args.workspace,
            event_type: args.event_type,
            role: args.role,
            agent_type: args.agent_type,
            file: args.file,
            cursor: args.cursor,
            limit: args.limit,
            format: match args.format {
                EventQueryFormat::Json => OutputFormat::Json,
                EventQueryFormat::Jsonl => OutputFormat::Jsonl,
            },
            scope: match args.scope {
                EventQueryScope::All => ListEventsScope::All,
                EventQueryScope::Primary => ListEventsScope::Primary,
                EventQueryScope::Subagent => ListEventsScope::Subagent,
            },
            direction: match args.direction {
                EventQueryDirection::Ascending => ListEventsDirection::Ascending,
                EventQueryDirection::Descending => ListEventsDirection::Descending,
            },
            content: match args.content {
                EventContentProjectionArg::Full => ListEventsContentProjection::Full,
                EventContentProjectionArg::Text => ListEventsContentProjection::Text,
                EventContentProjectionArg::None => ListEventsContentProjection::None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ListEventsArgs;
    use crate::ListEventsRequest;

    #[test]
    fn owned_list_conversion_reuses_request_buffers() {
        let provider = "codex provider".to_owned();
        let provider_pointer = provider.as_ptr();
        let workspace = "workspace buffer".to_owned();
        let workspace_pointer = workspace.as_ptr();
        let request = ListEventsRequest::from(ListEventsArgs {
            provider: vec![provider],
            workspace: Some(workspace),
            ..ListEventsArgs::default()
        });

        assert_eq!(request.providers[0].as_ptr(), provider_pointer);
        assert_eq!(
            request.workspace.as_deref().unwrap().as_ptr(),
            workspace_pointer
        );
    }
}
