use clap::{Args, ValueEnum};
use ctx_history_index::{CoreEventRangeDirection, CoreEventRangeScope};

use super::DEFAULT_EVENT_QUERY_LIMIT;
pub(crate) use ctx_history_read_application::{EventContentProjection, EventQueryWireRequest};

#[derive(Debug, Args)]
pub(crate) struct ListEventsArgs {
    #[arg(
        long,
        requires = "until",
        help = "Inclusive millisecond-aligned absolute RFC3339 lower bound"
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        requires = "since",
        help = "Exclusive millisecond-aligned absolute RFC3339 upper bound"
    )]
    pub(crate) until: Option<String>,
    #[arg(
        long,
        help = "Filter by exact provider; repeat to select more than one"
    )]
    pub(crate) provider: Vec<String>,
    #[arg(long, help = "Filter by exact public ctx source UUID")]
    pub(crate) source: Option<String>,
    #[arg(
        long = "history-source",
        help = "Filter custom history source as provider-key/source-id"
    )]
    pub(crate) history_source: Option<String>,
    #[arg(long = "provider-key", help = "Filter by custom history provider key")]
    pub(crate) provider_key: Option<String>,
    #[arg(long = "source-id", help = "Filter by custom history source ID")]
    pub(crate) source_id: Option<String>,
    #[arg(long = "source-format", help = "Filter by exact indexed source format")]
    pub(crate) source_format: Option<String>,
    #[arg(
        long = "provider-session",
        help = "Filter by exact provider-native session ID"
    )]
    pub(crate) provider_session: Option<String>,
    #[arg(long, help = "Filter by exact public ctx session UUID")]
    pub(crate) session: Option<String>,
    #[arg(
        long = "parent-session",
        help = "Filter by exact public parent ctx session UUID"
    )]
    pub(crate) parent_session: Option<String>,
    #[arg(
        long = "root-session",
        help = "Filter by exact public root ctx session UUID"
    )]
    pub(crate) root_session: Option<String>,
    #[arg(long, help = "Filter by exact branch")]
    pub(crate) branch: Option<String>,
    #[arg(long, help = "Filter by case-insensitive workspace or cwd substring")]
    pub(crate) workspace: Option<String>,
    #[arg(
        long = "event-type",
        help = "Filter by exact event type, including provider-defined values"
    )]
    pub(crate) event_type: Option<String>,
    #[arg(long, help = "Filter by exact role")]
    pub(crate) role: Option<String>,
    #[arg(long = "agent-type", help = "Filter by exact agent type")]
    pub(crate) agent_type: Option<String>,
    #[arg(long, value_enum, default_value_t = EventQueryScope::All)]
    pub(crate) scope: EventQueryScope,
    #[arg(long, help = "Filter by case-insensitive touched-file substring")]
    pub(crate) file: Option<String>,
    #[arg(long, value_enum, default_value_t = EventQueryDirection::Ascending)]
    pub(crate) direction: EventQueryDirection,
    #[arg(long, help = "Resume from an opaque cursor returned by a prior page")]
    pub(crate) cursor: Option<String>,
    #[arg(long, default_value_t = DEFAULT_EVENT_QUERY_LIMIT, help = "Maximum events returned across the complete invocation")]
    pub(crate) limit: u64,
    #[arg(long, value_enum, default_value_t = EventContentProjectionArg::Full)]
    pub(crate) content: EventContentProjectionArg,
    #[arg(long, value_enum, default_value_t = EventQueryFormat::Json)]
    pub(crate) format: EventQueryFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryFormat {
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventContentProjectionArg {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryScope {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryDirection {
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
