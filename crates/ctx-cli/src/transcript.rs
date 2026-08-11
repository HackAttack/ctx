use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TranscriptMode {
    Full,
    Lite,
    Log,
}

pub(crate) use ctx_history_cli::{shell_quote_arg, write_output, TranscriptOutput};

#[cfg(test)]
pub(crate) fn normalize_uuid_prefix(value: &str, kind: &str) -> anyhow::Result<String> {
    ctx_history_read_application::normalize_uuid_prefix(value).map_err(|error| match error {
        ctx_history_read_application::UuidPrefixError::TooShort => anyhow::anyhow!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        ),
        ctx_history_read_application::UuidPrefixError::InvalidHex => anyhow::anyhow!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ),
    })
}
