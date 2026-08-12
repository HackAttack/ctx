use clap::Args;

use crate::{output::JsonOutputFormat, progress::ProgressArg};

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(
        long,
        alias = "no-import",
        help = "Deprecated and ignored; setup follows its normal refresh lifecycle"
    )]
    pub catalog_only: bool,
    #[arg(
        long,
        help = "Enable local semantic search in config (requires daemon maintenance)"
    )]
    pub semantic: bool,
    #[arg(long, help = "Do not start daemon maintenance after setup")]
    pub no_daemon: bool,
    #[arg(
        long,
        help = "Wait for the daemon-owned lexical refresh to publish before returning"
    )]
    pub wait: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub progress: ProgressArg,
}
