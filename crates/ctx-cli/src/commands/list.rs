use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::{analytics::ShowTelemetry, local_usage::CliUsage, ui::Ui, ListArgs, ListTarget};

pub(crate) mod events {
    use clap::{Args, ValueEnum};

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
        #[arg(long, default_value_t = ctx_history_cli::DEFAULT_EVENT_QUERY_LIMIT, help = "Maximum events returned across the complete invocation")]
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub(crate) enum EventQueryScope {
        All,
        Primary,
        Subagent,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
    pub(crate) enum EventQueryDirection {
        Ascending,
        Descending,
    }

    pub(crate) use ctx_history_cli::{
        decode_cursor, event_query_error_value, event_range_page_value,
        list_events_selection as selection, mcp_event_query_core_record_bytes, render_event,
        validated_event_limit as validated_limit, EventContentProjection, EventQueryError,
        EventQueryWireRequest,
    };
}

pub(crate) use events::{EventQueryFormat, ListEventsArgs};

pub(crate) fn run_list(
    args: ListArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match args.target {
        ListTarget::Events(args) => {
            ctx_history_cli::run_list_events(adapt(*args), data_root, telemetry, local_usage, ui)
        }
    }
}

fn adapt(args: ListEventsArgs) -> ctx_history_cli::ListEventsArgs {
    ctx_history_cli::ListEventsArgs {
        since: args.since,
        until: args.until,
        provider: args.provider,
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
        scope: match args.scope {
            events::EventQueryScope::All => ctx_history_cli::EventQueryScope::All,
            events::EventQueryScope::Primary => ctx_history_cli::EventQueryScope::Primary,
            events::EventQueryScope::Subagent => ctx_history_cli::EventQueryScope::Subagent,
        },
        file: args.file,
        direction: match args.direction {
            events::EventQueryDirection::Ascending => {
                ctx_history_cli::EventQueryDirection::Ascending
            }
            events::EventQueryDirection::Descending => {
                ctx_history_cli::EventQueryDirection::Descending
            }
        },
        cursor: args.cursor,
        limit: args.limit,
        content: match args.content {
            events::EventContentProjectionArg::Full => {
                ctx_history_cli::EventContentProjectionArg::Full
            }
            events::EventContentProjectionArg::Text => {
                ctx_history_cli::EventContentProjectionArg::Text
            }
            events::EventContentProjectionArg::None => {
                ctx_history_cli::EventContentProjectionArg::None
            }
        },
        format: match args.format {
            events::EventQueryFormat::Json => ctx_history_cli::EventQueryFormat::Json,
            events::EventQueryFormat::Jsonl => ctx_history_cli::EventQueryFormat::Jsonl,
        },
    }
}
