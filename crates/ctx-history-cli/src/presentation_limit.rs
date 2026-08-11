//! Shared presentation limits are owned by the terminal package.

pub use ctx_terminal::{
    enforce_presentation_cli_output_limit, enforce_presentation_output_limit,
    serialized_json_bytes, PresentationOutputLimitError, CLI_PRESENTATION_MAX_OUTPUT_BYTES,
};
