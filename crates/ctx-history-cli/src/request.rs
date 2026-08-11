use std::path::PathBuf;

/// Provider identity after parsing. Parser spelling and aliases remain a final
/// `ctx` concern; this value preserves the canonical provider identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryProvider {
    Native(String),
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Jsonl,
    Markdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Plain,
    Json,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptMode {
    Full,
    Lite,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    Background,
    Off,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    CtxHistoryJsonlV1,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub terms: Vec<String>,
    pub limit: usize,
    pub provider: Option<HistoryProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub include_subagents: bool,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub semantic_weight: f32,
    pub refresh: RefreshMode,
    pub include_current_session: bool,
    pub format: OutputFormat,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowRequest {
    Session {
        id: Option<String>,
        provider: Option<HistoryProvider>,
        provider_session: Option<String>,
        mode: TranscriptMode,
        max_events: Option<usize>,
        format: OutputFormat,
        out: Option<PathBuf>,
    },
    Event {
        id: String,
        before: usize,
        after: usize,
        window: Option<usize>,
        format: OutputFormat,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRequest {
    Session {
        id: Option<String>,
        provider: Option<HistoryProvider>,
        provider_session: Option<String>,
        format: OutputFormat,
    },
    Event { id: String, format: OutputFormat },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRequest {
    pub events: ListEventsRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEventsRequest {
    pub since: Option<String>,
    pub until: Option<String>,
    pub providers: Vec<String>,
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
    pub file: Option<String>,
    pub cursor: Option<String>,
    pub limit: u64,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub provider: Option<HistoryProvider>,
    pub path: Option<PathBuf>,
    pub relocate_from: Option<PathBuf>,
    pub history_source: Option<String>,
    pub history_source_manifests: Vec<PathBuf>,
    pub reset_cursor: bool,
    pub input_format: Option<ImportFormat>,
    pub all: bool,
    pub resume: bool,
    pub partial: bool,
    pub no_daemon: bool,
    pub format: OutputFormat,
    pub progress: ProgressMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRequest {
    pub catalog_only: bool,
    pub semantic: bool,
    pub no_daemon: bool,
    pub wait: bool,
    pub format: OutputFormat,
    pub progress: ProgressMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcesRequest {
    pub provider: Option<HistoryProvider>,
    pub all: bool,
    pub show_missing: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceIndexRequest {
    Search(SearchRequest),
    Show(ShowRequest),
    Locate(LocateRequest),
    List(ListRequest),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_index_request_keeps_transport_modes_neutral() {
        let request = SourceIndexRequest::Show(ShowRequest::Session {
            id: Some("session".to_owned()),
            provider: Some(HistoryProvider::Native("codex".to_owned())),
            provider_session: None,
            mode: TranscriptMode::Lite,
            max_events: None,
            format: OutputFormat::Markdown,
            out: Some(PathBuf::from("out.md")),
        });

        assert!(matches!(request, SourceIndexRequest::Show(_)));
    }
}
