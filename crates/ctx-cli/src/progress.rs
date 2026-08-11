//! CLI argument aliases for terminal progress.

use clap::ValueEnum;
use ctx_history_refresh::RefreshStatus;

pub(crate) use ctx_terminal::{format_bytes, format_count, ProgressWriterError};
use ctx_terminal::{RefreshProgressSnapshot, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProgressArg {
    Auto,
    Plain,
    Json,
    None,
}

impl From<ProgressArg> for ctx_terminal::ProgressMode {
    fn from(value: ProgressArg) -> Self {
        match value {
            ProgressArg::Auto => Self::Auto,
            ProgressArg::Plain => Self::Plain,
            ProgressArg::Json => Self::Json,
            ProgressArg::None => Self::None,
        }
    }
}

/// CLI composition adapter. It preserves the established `RefreshStatus`
/// callback interface while converting validated engine data to the terminal
/// crate's neutral snapshot before output is rendered.
pub(crate) struct ProgressReporter<'a>(ctx_terminal::ProgressReporter<'a>);

impl<'a> ProgressReporter<'a> {
    pub(crate) fn new(
        ui: &'a mut Ui,
        arg: ProgressArg,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        Self(ctx_terminal::ProgressReporter::new(
            ui,
            arg.into(),
            json_output,
            operation,
            total_bytes,
        ))
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    pub(crate) fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        self.0.message(phase, message)
    }

    pub(crate) fn source_refresh(
        &mut self,
        status: &RefreshStatus,
    ) -> Result<(), ProgressWriterError> {
        let snapshot = RefreshProgressSnapshot::from_schema_v1(&status.schema_v1_fields())
            .map_err(|error| ProgressWriterError::from(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?;
        self.0.source_refresh(snapshot)
    }
}
