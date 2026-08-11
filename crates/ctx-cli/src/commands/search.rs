use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::analytics::{
    count_bucket, duration_bucket, text_length_bucket, RefreshMode, RefreshStatus, SearchBackend,
    SearchTelemetry,
};
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
    config: &config::AppConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let observation = ctx_history_cli::run_search(
        adapt(args),
        data_root,
        ctx_history_cli::HistoryCliConfig {
            daemon_enabled: config.daemon.enabled,
            semantic_search_enabled: config.semantic_search_enabled(),
            local_usage_enabled: config.local_usage.enabled,
        },
        local_usage,
        ui,
    )?;
    apply_search_observation(telemetry, observation);
    Ok(())
}

pub(crate) fn adapt(args: SearchArgs) -> ctx_history_cli::SearchArgs {
    let provider = args.provider.map(|provider| {
        ctx_history_cli::ProviderArg(ctx_history_cli::HistoryProvider::from(
            provider.capture_provider(),
        ))
    });
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
    }
}

fn apply_search_observation(
    telemetry: &mut SearchTelemetry,
    observation: ctx_history_cli::SearchExecutionObservation,
) {
    telemetry.refresh_mode = Some(match observation.refresh_mode {
        ctx_history_cli::RefreshMode::Background => RefreshMode::Background,
        ctx_history_cli::RefreshMode::Off => RefreshMode::Off,
        ctx_history_cli::RefreshMode::Wait => RefreshMode::Wait,
    });
    telemetry.refresh_status = Some(match observation.refresh_status {
        ctx_history_cli::SearchRefreshStatus::ExistingGeneration => {
            RefreshStatus::from_safe_summary("existing_generation")
        }
        ctx_history_cli::SearchRefreshStatus::DaemonBackground => {
            RefreshStatus::from_safe_summary("daemon_background")
        }
        ctx_history_cli::SearchRefreshStatus::DaemonUnavailable => {
            RefreshStatus::from_safe_summary("daemon_unavailable")
        }
        ctx_history_cli::SearchRefreshStatus::Completed => {
            RefreshStatus::from_safe_summary("completed")
        }
    });
    telemetry.refresh_source_count = Some(count_bucket(observation.refresh_source_count));
    telemetry.refresh_duration = Some(duration_bucket(observation.refresh_duration));
    telemetry.query_duration = Some(duration_bucket(observation.query_duration));
    telemetry.render_duration = Some(duration_bucket(observation.render_duration));
    telemetry.backend_requested = Some(search_backend(observation.backend_requested));
    telemetry.backend_effective = Some(search_backend(observation.backend_effective));
    telemetry.result_count = Some(count_bucket(observation.result_count));
    telemetry.citation_count = Some(count_bucket(observation.citation_count));
    telemetry.zero_result = Some(observation.zero_result);
    telemetry.has_indexed_content_after = Some(observation.has_indexed_content_after);
    telemetry.query_length = Some(text_length_bucket(observation.query_length as usize));
    telemetry.query_term_count = Some(count_bucket(observation.query_term_count));
}

const fn search_backend(value: ctx_history_read_application::SearchBackend) -> SearchBackend {
    match value {
        ctx_history_read_application::SearchBackend::Hybrid => SearchBackend::Hybrid,
        ctx_history_read_application::SearchBackend::Lexical => SearchBackend::Lexical,
        ctx_history_read_application::SearchBackend::Semantic => SearchBackend::Semantic,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser;

    use super::*;
    use crate::{cli::CommandRoot, Cli};

    fn telemetry() -> SearchTelemetry {
        SearchTelemetry {
            has_query: true,
            has_provider_filter: false,
            has_workspace_filter: false,
            has_since_filter: false,
            has_event_type_filter: false,
            has_file_filter: false,
            has_session_filter: false,
            event_results: false,
            primary_only: false,
            include_subagents: false,
            include_current_session: false,
            limit: count_bucket(10),
            provider_filter: None,
            refresh_duration: None,
            refresh_mode: None,
            refresh_status: None,
            refresh_source_count: None,
            has_indexed_content_after: None,
            query_length: None,
            query_term_count: None,
            query_duration: None,
            backend_requested: None,
            backend_effective: None,
            result_count: None,
            citation_count: None,
            zero_result: None,
            render_duration: None,
        }
    }

    #[test]
    fn observation_mapping_populates_every_final_analytics_field_once() {
        let mut telemetry = telemetry();
        apply_search_observation(
            &mut telemetry,
            ctx_history_cli::SearchExecutionObservation {
                refresh_mode: ctx_history_cli::RefreshMode::Wait,
                refresh_status: ctx_history_cli::SearchRefreshStatus::Completed,
                refresh_source_count: 3,
                refresh_duration: Duration::from_millis(1),
                query_duration: Duration::from_millis(2),
                render_duration: Duration::from_millis(3),
                backend_requested: ctx_history_read_application::SearchBackend::Hybrid,
                backend_effective: ctx_history_read_application::SearchBackend::Lexical,
                result_count: 4,
                citation_count: 5,
                zero_result: false,
                has_indexed_content_after: true,
                query_length: 6,
                query_term_count: 2,
            },
        );

        assert_eq!(telemetry.refresh_mode, Some(RefreshMode::Wait));
        assert_eq!(telemetry.refresh_status, Some(RefreshStatus::Completed));
        assert_eq!(telemetry.backend_requested, Some(SearchBackend::Hybrid));
        assert_eq!(telemetry.backend_effective, Some(SearchBackend::Lexical));
        assert_eq!(telemetry.zero_result, Some(false));
        assert_eq!(telemetry.has_indexed_content_after, Some(true));
        assert!(telemetry.refresh_source_count.is_some());
        assert!(telemetry.refresh_duration.is_some());
        assert!(telemetry.query_duration.is_some());
        assert!(telemetry.render_duration.is_some());
        assert!(telemetry.query_length.is_some());
        assert!(telemetry.query_term_count.is_some());
        assert!(telemetry.result_count.is_some());
        assert!(telemetry.citation_count.is_some());
    }

    #[test]
    fn search_clap_adapter_accepts_omitted_and_explicit_all_scope() {
        let parsed = Cli::try_parse_from(["ctx", "search", "needle"]).unwrap();
        let CommandRoot::Search(omitted) = parsed.command else {
            panic!("expected search command");
        };
        let parsed =
            Cli::try_parse_from(["ctx", "search", "needle", "--content-scope", "all"]).unwrap();
        let CommandRoot::Search(explicit) = parsed.command else {
            panic!("expected search command");
        };
        assert_eq!(omitted.query.as_deref(), Some("needle"));
        assert_eq!(omitted.content_scope, None);
        assert!(matches!(explicit.content_scope, Some(ContentScopeArg::All)));
    }

    #[test]
    fn search_clap_adapter_forwards_content_scope_controls() {
        let parsed = Cli::try_parse_from([
            "ctx",
            "search",
            "needle",
            "--content-scope",
            "calls",
            "--events",
            "--include-subagents",
            "--include-current-session",
        ])
        .unwrap();
        let CommandRoot::Search(args) = parsed.command else {
            panic!("expected search command");
        };
        assert!(matches!(args.content_scope, Some(ContentScopeArg::Calls)));
        assert!(args.events && args.include_subagents && args.include_current_session);
    }

    #[test]
    fn empty_search_action_is_a_valid_positional_query() {
        Cli::try_parse_from(["ctx", "search", "<term>"])
            .expect("empty-state action must be a valid positional search invocation");
    }
}
