use ctx_history_core::CoreRecord;

use super::*;

// The large variant deliberately carries CoreRecord by value: boxing every
// projected record would add one allocation to the generic JSONL hot path.
#[allow(clippy::large_enum_variant)]
pub(crate) enum JsonlLeafOutputEvent {
    Page {
        append: bool,
        completed_bytes: u64,
        records: Vec<CoreRecord>,
    },
    Record {
        append: bool,
        record: CoreRecord,
    },
    Flush,
}

pub(crate) struct JsonlLeafOutput<'emit, E: JsonlFamilyError> {
    emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> JsonlResult<(), E>,
}

impl<'emit, E: JsonlFamilyError> JsonlLeafOutput<'emit, E> {
    pub(crate) fn new(
        emit: &'emit mut dyn FnMut(JsonlLeafOutputEvent) -> JsonlResult<(), E>,
    ) -> Self {
        Self { emit }
    }

    pub(crate) fn emit_page(
        &mut self,
        append: bool,
        completed_bytes: u64,
        records: Vec<CoreRecord>,
    ) -> JsonlResult<(), E> {
        (self.emit)(JsonlLeafOutputEvent::Page {
            append,
            completed_bytes,
            records,
        })
    }

    pub(crate) fn emit_record(&mut self, append: bool, record: CoreRecord) -> JsonlResult<(), E> {
        (self.emit)(JsonlLeafOutputEvent::Record { append, record })
    }

    pub(crate) fn flush(&mut self) -> JsonlResult<(), E> {
        (self.emit)(JsonlLeafOutputEvent::Flush)
    }
}
