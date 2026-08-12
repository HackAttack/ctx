//! Non-Pro Clap adapters, command presentation, and terminal-facing workflows.
//!
//! The final `ctx` binary owns process startup, persisted configuration,
//! installation identity, daemon composition, release provenance, and Pro.
//! This crate receives those authorities through explicit per-call values and
//! ports; it never depends on the final binary.

// Keep direct CLI writes on the same measured stdout/stderr seam as structured
// terminal UI so analytics and local-usage byte accounting remain unchanged.
macro_rules! print {
    ($($arg:tt)*) => {{
        ctx_terminal::write_stdout(format_args!($($arg)*));
    }};
}

macro_rules! println {
    () => {{
        ctx_terminal::write_stdout_line(format_args!(""));
    }};
    ($($arg:tt)*) => {{
        ctx_terminal::write_stdout_line(format_args!($($arg)*));
    }};
}

macro_rules! eprintln {
    () => {{
        ctx_terminal::write_stderr_line(format_args!(""));
    }};
    ($($arg:tt)*) => {{
        ctx_terminal::write_stderr_line(format_args!($($arg)*));
    }};
}

pub mod docs;
pub mod integrations;
pub mod mcp_text;
pub mod analytics {
    pub use ctx_client_observability::analytics::*;
}
pub mod commands;
pub mod local_usage {
    pub use ctx_client_observability::local_usage::*;
}
pub mod output;
pub mod progress;
pub mod provider_args;
pub mod skill;
pub mod transcript;
pub mod value_parsers;
pub mod ui {
    pub use ctx_terminal::ui::*;
}

pub use output::{JsonOutputFormat, OutputFormat};
pub use progress::ProgressArg;
pub use provider_args::{
    cli_supported_provider, parse_native_provider_arg, parse_provider_arg, ImportFormatArg,
    NativeProviderArg, ProviderArg,
};
pub use transcript::TranscriptMode;
pub use value_parsers::{parse_daemon_interval_seconds, parse_event_window_limit};

/// Marks a command failure whose exact command-specific output was emitted.
#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub struct RenderedCliError;

pub fn rendered_cli_error() -> anyhow::Error {
    RenderedCliError.into()
}
