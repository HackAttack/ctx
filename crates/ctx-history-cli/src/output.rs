//! Neutral output vocabulary backed by the shared terminal package.

pub use crate::request::OutputFormat;
pub use ctx_terminal::compact_json;

pub fn print_json(value: serde_json::Value) -> anyhow::Result<()> {
    ctx_terminal::print_json(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOutputFormat {
    Text,
    Json,
}

impl JsonOutputFormat {
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}
