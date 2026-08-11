use anyhow::Result;
use ctx_history_index::{CopiedEventLineage, CoreEventRecord, SessionEventCursor, SessionRecord};
use serde_json::Value;

use crate::{
    output::OutputFormat, presentation_limit::PresentationOutputLimitError,
    transcript::TranscriptMode,
};

fn structured_format(format: OutputFormat) -> ctx_history_read_application::StructuredOutputFormat {
    match format {
        OutputFormat::Text => ctx_history_read_application::StructuredOutputFormat::Text,
        OutputFormat::Markdown => ctx_history_read_application::StructuredOutputFormat::Markdown,
        OutputFormat::Json => ctx_history_read_application::StructuredOutputFormat::Json,
        OutputFormat::Jsonl => ctx_history_read_application::StructuredOutputFormat::Jsonl,
    }
}

fn structured_mode(mode: TranscriptMode) -> ctx_history_read_application::StructuredTranscriptMode {
    match mode {
        TranscriptMode::Full => ctx_history_read_application::StructuredTranscriptMode::Full,
        TranscriptMode::Lite => ctx_history_read_application::StructuredTranscriptMode::Lite,
        TranscriptMode::Log => ctx_history_read_application::StructuredTranscriptMode::Log,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::commands::source_index) fn session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    rendered: Vec<Value>,
    truncated: bool,
    max_events: Option<usize>,
) -> Value {
    ctx_history_read_application::session_transcript_read_model(
        session,
        structured_mode(mode),
        structured_format(format),
        rendered,
        truncated,
        max_events,
    )
}

pub(super) fn event_window_json_with_lineage(
    selected: &CoreEventRecord,
    events: &[CoreEventRecord],
    copied_lineage: &CopiedEventLineage,
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    ctx_history_read_application::event_window_with_lineage_read_model(
        selected,
        events,
        copied_lineage,
        structured_format(format),
        output_limit_bytes,
    )
    .map_err(map_read_model_error)
}

pub(super) fn paginated_session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    events: Vec<Value>,
    limit: usize,
    has_more: bool,
    next_cursor: Option<&SessionEventCursor>,
) -> Result<Value> {
    ctx_history_read_application::paginated_session_transcript_read_model(
        session,
        structured_mode(mode),
        structured_format(format),
        events,
        limit,
        has_more,
        next_cursor,
    )
}

#[cfg(test)]
pub(in crate::commands::source_index) fn event_window_value(
    selected: &CoreEventRecord,
    format: OutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    ctx_history_read_application::event_window_value(selected, structured_format(format), rendered)
}

#[cfg(test)]
pub(in crate::commands::source_index) fn render_event_values(
    events: &[&CoreEventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    ctx_history_read_application::render_event_read_model_values(events, output_limit_bytes)
        .map_err(map_read_model_error)
}

pub(in crate::commands::source_index) fn render_event_value(event: &CoreEventRecord) -> Value {
    ctx_history_read_application::render_show_event_read_model(event)
}

fn map_read_model_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<ctx_history_read_application::ReadModelLimitError>() {
        Ok(error) => anyhow::Error::new(PresentationOutputLimitError {
            event_id: error.event_id,
            actual_bytes: error.actual_bytes,
            maximum_bytes: error.maximum_bytes,
        }),
        Err(error) => error,
    }
}
