use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::{
    analytics::{count_bucket, SourcesTelemetry},
    local_usage::{CliUsage, ResultObservationAction},
    output::JsonOutputFormat,
};

#[derive(Debug, Args, Clone)]
pub struct SourcesArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(
        long,
        value_parser = crate::parse_provider_arg,
        hide_possible_values = true,
        help = "Show sources for one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub provider: Option<crate::ProviderArg>,
    #[arg(long, help = "Show every supported provider location")]
    pub all: bool,
    #[arg(long, help = "Show missing locations for every known provider")]
    pub show_missing: bool,
}

/// Final-host shell for the sources command. Clap conversion and result delivery
/// remain here; application execution and presentation live in `ctx-history-cli`.
pub fn run_sources(
    args: SourcesArgs,
    data_root: PathBuf,
    telemetry: &mut SourcesTelemetry,
    local_usage: &mut CliUsage,
    home_dir: Option<PathBuf>,
    ui: &mut ctx_terminal::Ui,
) -> Result<()> {
    let request = ctx_history_cli::SourcesRequest {
        provider: args
            .provider
            .map(|provider| provider.capture_provider().into()),
        all: args.all,
        show_missing: args.show_missing,
        format: match args.format {
            JsonOutputFormat::Text => ctx_history_cli::OutputFormat::Text,
            JsonOutputFormat::Json => ctx_history_cli::OutputFormat::Json,
        },
    };
    let observation = ctx_history_cli::run_sources(
        request,
        &data_root,
        home_dir,
        |observation| {
            telemetry.providers_detected = Some(count_bucket(observation.providers_detected));
            telemetry.providers_existing = Some(count_bucket(observation.providers_existing));
            telemetry.providers_importable = Some(count_bucket(observation.providers_importable));
        },
        ui,
    )?;
    local_usage.set_result_observation(
        ResultObservationAction::Sources,
        observation.result_count,
        0,
        observation.content_bytes,
    );
    local_usage.set_measured_output_bytes(observation.output_bytes);
    Ok(())
}
