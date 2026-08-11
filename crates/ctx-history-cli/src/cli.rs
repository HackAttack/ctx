//! Plain parsed command values. Clap conversion remains in the final binary.

use std::path::PathBuf;

use crate::provider_args::ProviderArg;
use crate::{
    output::JsonOutputFormat, HistoryProvider, OutputFormat, RefreshMode, SearchBackend,
    SearchContentScope, SearchRequest, TranscriptMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackendArg {
    Hybrid,
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentScopeArg {
    All,
    Transcript,
    Calls,
    Outputs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchArgs {
    pub query: Option<String>,
    pub term: Vec<String>,
    pub limit: usize,
    pub provider: Option<ProviderArg>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub include_subagents: bool,
    pub content_scope: Option<ContentScopeArg>,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub backend: Option<SearchBackendArg>,
    pub semantic_weight: f32,
    pub refresh: RefreshMode,
    pub include_current_session: bool,
    pub format: JsonOutputFormat,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowArgs {
    pub target: ShowTarget,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowTarget {
    Session(ShowSessionArgs),
    Event(ShowEventArgs),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowSessionArgs {
    pub id: Option<String>,
    pub provider: Option<ProviderArg>,
    pub provider_session: Option<String>,
    pub mode: TranscriptMode,
    pub max_events: Option<usize>,
    pub format: OutputFormat,
    pub out: Option<PathBuf>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowEventArgs {
    pub id: String,
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateArgs {
    pub target: LocateTarget,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateTarget {
    Session(LocateSessionArgs),
    Event(LocateEventArgs),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateSessionArgs {
    pub id: Option<String>,
    pub provider: Option<ProviderArg>,
    pub provider_session: Option<String>,
    pub format: JsonOutputFormat,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateEventArgs {
    pub id: String,
    pub format: JsonOutputFormat,
}

impl From<&SearchArgs> for SearchRequest {
    fn from(args: &SearchArgs) -> Self {
        Self {
            query: args.query.clone(),
            terms: args.term.clone(),
            limit: args.limit,
            provider: args.provider.clone().map(|value| value.0),
            history_source: args.history_source.clone(),
            provider_key: args.provider_key.clone(),
            source_id: args.source_id.clone(),
            source_format: args.source_format.clone(),
            workspace: args.workspace.clone(),
            since: args.since.clone(),
            primary_only: args.primary_only,
            include_subagents: args.include_subagents,
            content_scope: match args.content_scope.unwrap_or(ContentScopeArg::All) {
                ContentScopeArg::All => SearchContentScope::All,
                ContentScopeArg::Transcript => SearchContentScope::Transcript,
                ContentScopeArg::Calls => SearchContentScope::Calls,
                ContentScopeArg::Outputs => SearchContentScope::Outputs,
            },
            event_type: args.event_type.clone(),
            file: args.file.clone(),
            session: args.session.clone(),
            events: args.events,
            backend: args.backend.map(|value| match value {
                SearchBackendArg::Hybrid => SearchBackend::Hybrid,
                SearchBackendArg::Lexical => SearchBackend::Lexical,
                SearchBackendArg::Semantic => SearchBackend::Semantic,
            }),
            semantic_weight: args.semantic_weight,
            refresh: args.refresh,
            include_current_session: args.include_current_session,
            format: match args.format {
                JsonOutputFormat::Text => OutputFormat::Text,
                JsonOutputFormat::Json => OutputFormat::Json,
            },
            verbose: args.verbose,
        }
    }
}
