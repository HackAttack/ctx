use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::analytics::SearchTelemetry;
use crate::commands::import::ProviderRefreshCollector;
use crate::local_usage::CliUsage;
use crate::ui::Ui;
use crate::{
    cli::{CliSearchBackendArg, ContentScopeArg},
    config, SearchArgs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CliRefreshArg {
    Background,
    Off,
    Wait,
}

impl CliRefreshArg {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    _provider_refreshes: &mut ProviderRefreshCollector,
    _config: &config::AppConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let mut observations = SearchObservationCollector::default();
    let provider = args.provider.map(|provider| {
        let capture = provider.capture_provider();
        ctx_history_cli::ProviderArg(if capture == ctx_history_core::CaptureProvider::Custom {
            ctx_history_cli::HistoryProvider::Custom
        } else {
            ctx_history_cli::HistoryProvider::Native(capture.as_str().to_owned())
        })
    });
    ctx_history_cli::run_search_with_observations(
        ctx_history_cli::SearchArgs {
            query: args.query,
            term: args.term,
            limit: args.limit,
            provider,
            history_source: args.history_source,
            provider_key: args.provider_key,
            source_id: args.source_id,
            source_format: args.source_format,
            workspace: args.workspace,
            since: args.since,
            primary_only: args.primary_only,
            include_subagents: args.include_subagents,
            content_scope: args.content_scope.map(|scope| match scope {
                ContentScopeArg::All => ctx_history_cli::ContentScopeArg::All,
                ContentScopeArg::Transcript => ctx_history_cli::ContentScopeArg::Transcript,
                ContentScopeArg::Calls => ctx_history_cli::ContentScopeArg::Calls,
                ContentScopeArg::Outputs => ctx_history_cli::ContentScopeArg::Outputs,
            }),
            event_type: args.event_type,
            file: args.file,
            session: args.session,
            events: args.events,
            backend: args.backend.map(|backend| match backend {
                CliSearchBackendArg::Hybrid => ctx_history_cli::SearchBackendArg::Hybrid,
                CliSearchBackendArg::Lexical => ctx_history_cli::SearchBackendArg::Lexical,
                CliSearchBackendArg::Semantic => ctx_history_cli::SearchBackendArg::Semantic,
            }),
            semantic_weight: args.semantic_weight,
            refresh: match args.refresh {
                CliRefreshArg::Background => ctx_history_cli::RefreshMode::Background,
                CliRefreshArg::Off => ctx_history_cli::RefreshMode::Off,
                CliRefreshArg::Wait => ctx_history_cli::RefreshMode::Wait,
            },
            include_current_session: args.include_current_session,
            format: match args.format {
                crate::output::JsonOutputFormat::Text => ctx_history_cli::JsonOutputFormat::Text,
                crate::output::JsonOutputFormat::Json => ctx_history_cli::JsonOutputFormat::Json,
            },
            verbose: args.verbose,
        },
        data_root,
        telemetry,
        local_usage,
        ui,
        &mut observations,
    )
}

#[derive(Default)]
struct SearchObservationCollector {
    observation: Option<ctx_history_cli::HistoryCliObservation>,
}

impl ctx_history_cli::ObservabilityPort for SearchObservationCollector {
    fn observe(&mut self, observation: ctx_history_cli::HistoryCliObservation) {
        self.observation = Some(observation);
    }
}
