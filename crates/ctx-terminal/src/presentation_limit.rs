use std::{fmt, io};

use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub const CLI_PRESENTATION_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct SerializedByteCounter {
    bytes: usize,
}

impl io::Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationOutputLimitError {
    pub event_id: Uuid,
    pub actual_bytes: usize,
    pub maximum_bytes: usize,
}

impl fmt::Display for PresentationOutputLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core content output for ctx event {} requires {} bytes; the presentation limit is {} bytes",
            self.event_id, self.actual_bytes, self.maximum_bytes
        )
    }
}

impl std::error::Error for PresentationOutputLimitError {}

pub fn enforce_presentation_output_limit(
    serialized_output_bytes: usize,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), PresentationOutputLimitError> {
    if serialized_output_bytes > output_limit_bytes {
        return Err(PresentationOutputLimitError {
            event_id,
            actual_bytes: serialized_output_bytes,
            maximum_bytes: output_limit_bytes,
        });
    }
    Ok(())
}

pub fn enforce_presentation_cli_output_limit(
    rendered_output: &str,
    writes_stdout: bool,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<(), PresentationOutputLimitError> {
    let serialized_output_bytes = rendered_output.len().saturating_add(usize::from(
        writes_stdout && !rendered_output.ends_with('\n'),
    ));
    enforce_presentation_output_limit(serialized_output_bytes, output_limit_bytes, event_id)
}

pub fn serialized_json_bytes<T>(value: &T) -> serde_json::Result<usize>
where
    T: Serialize + ?Sized,
{
    let mut counter = SerializedByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

pub fn presentation_output_limit_error_json(error: &PresentationOutputLimitError) -> Value {
    json!({
        "error": "output_limit_exceeded",
        "error_code": "output_limit_exceeded",
        "ctx_event_id": error.event_id,
        "actual_bytes": error.actual_bytes,
        "maximum_bytes": error.maximum_bytes,
        "retryable": false,
        "remediation": "reduce the event window or choose a narrower transcript mode",
    })
}
