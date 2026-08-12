use clap::{Args, ValueEnum};

use crate::output::JsonOutputFormat;

#[derive(Debug, Args, Clone)]
pub struct StatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(
        long,
        value_enum,
        help = "Local usage control: enable, disable, or reset"
    )]
    pub usage: Option<UsageStatusMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UsageStatusMode {
    Enable,
    Disable,
    Reset,
}

impl UsageStatusMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Reset => "reset",
        }
    }
}
