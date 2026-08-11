//! Final CLI parsing adapter for neutral history progress reporting.

use clap::ValueEnum;

pub(crate) use ctx_history_cli::{
    format_bytes, format_count, presentation_snapshot, ProgressReporter, ProgressWriterError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProgressArg {
    Auto,
    Plain,
    Json,
    None,
}

impl From<ProgressArg> for ctx_history_cli::ProgressMode {
    fn from(value: ProgressArg) -> Self {
        match value {
            ProgressArg::Auto => Self::Auto,
            ProgressArg::Plain => Self::Plain,
            ProgressArg::Json => Self::Json,
            ProgressArg::None => Self::None,
        }
    }
}
