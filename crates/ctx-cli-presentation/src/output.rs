//! CLI composition aliases for terminal output.
//!
//! The argument enums live here because Clap is a composition concern; all
//! terminal writing and JSON helpers are implemented by `ctx-terminal`.

use clap::ValueEnum;

pub use ctx_terminal::output::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

impl From<OutputFormat> for ctx_terminal::OutputFormat {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Text => Self::Text,
            OutputFormat::Markdown => Self::Markdown,
            OutputFormat::Json => Self::Json,
            OutputFormat::Jsonl => Self::Jsonl,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum JsonOutputFormat {
    Text,
    Json,
}

impl From<JsonOutputFormat> for ctx_terminal::JsonOutputFormat {
    fn from(value: JsonOutputFormat) -> Self {
        match value {
            JsonOutputFormat::Text => Self::Text,
            JsonOutputFormat::Json => Self::Json,
        }
    }
}

impl JsonOutputFormat {
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}
