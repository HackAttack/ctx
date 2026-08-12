use std::{
    fs::File,
    io::{BufReader, Seek, SeekFrom},
};

use sha2::{Digest, Sha256};

use super::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_complete_sha256, read_bounded_record_full_complete_and_prefix_sha256,
    JsonlRecordFraming,
};
use crate::{CaptureError, Result};

#[derive(Debug, Clone)]
pub(crate) enum JsonlPhysicalDigest {
    Complete {
        complete: Sha256,
    },
    FullAndComplete {
        full: Sha256,
        complete: Sha256,
    },
    CompleteAndBoundedPrefix {
        complete: Sha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    },
    FullCompleteAndBoundedPrefix {
        full: Sha256,
        complete: Sha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    },
}

impl JsonlPhysicalDigest {
    pub(crate) fn complete(complete: Sha256) -> Self {
        Self::Complete { complete }
    }

    pub(crate) fn complete_and_bounded_prefix(
        complete: Sha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    ) -> Self {
        Self::CompleteAndBoundedPrefix {
            complete,
            bounded_prefix,
            bounded_prefix_remaining,
        }
    }

    pub(crate) fn full_and_complete(full: Sha256, complete: Sha256) -> Self {
        Self::FullAndComplete { full, complete }
    }

    pub(crate) fn full_complete_and_bounded_prefix(
        full: Sha256,
        complete: Sha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    ) -> Self {
        Self::FullCompleteAndBoundedPrefix {
            full,
            complete,
            bounded_prefix,
            bounded_prefix_remaining,
        }
    }

    pub(crate) fn complete_hasher(&self) -> &Sha256 {
        match self {
            Self::Complete { complete }
            | Self::FullAndComplete { complete, .. }
            | Self::CompleteAndBoundedPrefix { complete, .. }
            | Self::FullCompleteAndBoundedPrefix { complete, .. } => complete,
        }
    }

    pub(crate) fn full_hasher(&self) -> Option<&Sha256> {
        match self {
            Self::FullAndComplete { full, .. }
            | Self::FullCompleteAndBoundedPrefix { full, .. } => Some(full),
            _ => None,
        }
    }

    pub(crate) fn bounded_prefix(&self) -> Option<(&Sha256, u64)> {
        match self {
            Self::CompleteAndBoundedPrefix {
                bounded_prefix,
                bounded_prefix_remaining,
                ..
            } => Some((bounded_prefix, *bounded_prefix_remaining)),
            Self::FullCompleteAndBoundedPrefix {
                bounded_prefix,
                bounded_prefix_remaining,
                ..
            } => Some((bounded_prefix, *bounded_prefix_remaining)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlPhysicalRecord {
    pub(crate) physical_ordinal: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) complete: bool,
    pub(crate) terminal_nul_padding: bool,
    pub(crate) oversized: bool,
    pub(crate) stored_len: usize,
    pub(crate) sha256: [u8; 32],
}

impl JsonlPhysicalRecord {
    pub(crate) fn byte_len(self) -> u64 {
        self.byte_end_exclusive.saturating_sub(self.byte_start)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlPhysicalStreamPosition {
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    digest: JsonlPhysicalDigest,
    incomplete_tail: bool,
    exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JsonlPhysicalPassBinding {
    frozen_length: u64,
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    admitted_eof_sha256: Option<[u8; 32]>,
    incomplete_tail: bool,
    exhausted: bool,
}

#[derive(Debug)]
pub(crate) struct JsonlPhysicalStream {
    reader: BufReader<File>,
    frozen_length: u64,
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> CaptureError,
    digest: JsonlPhysicalDigest,
    record_buffer: Vec<u8>,
    incomplete_tail: bool,
    exhausted: bool,
}

impl JsonlPhysicalStream {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        mut file: File,
        frozen_length: u64,
        offset: u64,
        next_physical_ordinal: u64,
        framing: JsonlRecordFraming,
        digest: JsonlPhysicalDigest,
        source_changed: fn() -> CaptureError,
    ) -> Result<Self> {
        if offset > frozen_length {
            return Err(source_changed());
        }
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            reader: BufReader::new(file),
            frozen_length,
            offset,
            next_physical_ordinal,
            complete_prefix_end: offset,
            framing,
            source_changed,
            digest,
            record_buffer: Vec::new(),
            incomplete_tail: false,
            exhausted: false,
        })
    }

    pub(crate) fn next_record(&mut self) -> Result<Option<JsonlPhysicalRecord>> {
        if self.exhausted {
            return Ok(None);
        }
        if self.offset == self.frozen_length {
            self.exhausted = true;
            return Ok(None);
        }
        let remaining = self.frozen_length.saturating_sub(self.offset);
        let record = match &mut self.digest {
            JsonlPhysicalDigest::Complete { complete } => read_bounded_record_complete_sha256(
                &mut self.reader,
                &mut self.record_buffer,
                complete,
                remaining,
                self.framing,
                self.source_changed,
            )?,
            JsonlPhysicalDigest::FullAndComplete { full, complete } => read_bounded_record(
                &mut self.reader,
                &mut self.record_buffer,
                full,
                complete,
                remaining,
                self.framing,
                self.source_changed,
            )?,
            JsonlPhysicalDigest::CompleteAndBoundedPrefix {
                complete,
                bounded_prefix,
                bounded_prefix_remaining,
            } => read_bounded_record_complete_and_prefix_sha256(
                &mut self.reader,
                &mut self.record_buffer,
                complete,
                (bounded_prefix, bounded_prefix_remaining),
                remaining,
                self.framing,
                self.source_changed,
            )?,
            JsonlPhysicalDigest::FullCompleteAndBoundedPrefix {
                full,
                complete,
                bounded_prefix,
                bounded_prefix_remaining,
            } => read_bounded_record_full_complete_and_prefix_sha256(
                &mut self.reader,
                &mut self.record_buffer,
                full,
                complete,
                bounded_prefix,
                bounded_prefix_remaining,
                remaining,
                self.framing,
                self.source_changed,
            )?,
        }
        .ok_or_else(|| (self.source_changed)())?;
        let byte_start = self.offset;
        let byte_end_exclusive =
            byte_start
                .checked_add(record.byte_len)
                .ok_or(CaptureError::SystemInvariant(
                    "JSONL physical stream offset overflowed",
                ))?;
        self.offset = byte_end_exclusive;
        let physical_ordinal = self.next_physical_ordinal;
        if record.complete {
            self.complete_prefix_end = byte_end_exclusive;
            self.next_physical_ordinal =
                self.next_physical_ordinal
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "JSONL physical stream ordinal overflowed",
                    ))?;
        } else {
            self.incomplete_tail = true;
            self.exhausted = true;
        }
        Ok(Some(JsonlPhysicalRecord {
            physical_ordinal,
            byte_start,
            byte_end_exclusive,
            complete: record.complete,
            terminal_nul_padding: record.terminal_nul_padding,
            oversized: record.oversized,
            stored_len: record.stored_len,
            sha256: record.sha256,
        }))
    }

    pub(crate) fn record_bytes(&self, record: JsonlPhysicalRecord) -> &[u8] {
        &self.record_buffer[..record.stored_len]
    }

    /// Drops retained record capacity before a prepared page crosses a worker
    /// boundary. Providers that do not transfer pages keep the allocation.
    pub(crate) fn release_record_buffer(&mut self) {
        self.record_buffer = Vec::new();
    }

    pub(crate) fn position(&self) -> JsonlPhysicalStreamPosition {
        JsonlPhysicalStreamPosition {
            offset: self.offset,
            next_physical_ordinal: self.next_physical_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            digest: self.digest.clone(),
            incomplete_tail: self.incomplete_tail,
            exhausted: self.exhausted,
        }
    }

    pub(crate) fn restore(&mut self, position: JsonlPhysicalStreamPosition) -> Result<()> {
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.offset = position.offset;
        self.next_physical_ordinal = position.next_physical_ordinal;
        self.complete_prefix_end = position.complete_prefix_end;
        self.digest = position.digest;
        self.incomplete_tail = position.incomplete_tail;
        self.exhausted = position.exhausted;
        Ok(())
    }

    pub(crate) fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub(crate) fn digest(&self) -> &JsonlPhysicalDigest {
        &self.digest
    }

    pub(super) fn admitted_pass_binding(&self) -> JsonlPhysicalPassBinding {
        JsonlPhysicalPassBinding {
            frozen_length: self.frozen_length,
            offset: self.offset,
            next_physical_ordinal: self.next_physical_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: self.digest.complete_hasher().clone().finalize().into(),
            admitted_eof_sha256: self
                .digest
                .full_hasher()
                .map(|full| full.clone().finalize().into()),
            incomplete_tail: self.incomplete_tail,
            exhausted: self.exhausted,
        }
    }

    pub(crate) fn terminal(&self) -> bool {
        self.exhausted && !self.incomplete_tail
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn stream_tracks_complete_prefix_tail_and_rollback() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("records.jsonl");
        let contents = b"one\ntwo\nincomplete";
        std::fs::write(&path, contents).unwrap();
        let mut stream = JsonlPhysicalStream::open(
            File::open(&path).unwrap(),
            contents.len() as u64,
            0,
            0,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::full_and_complete(Sha256::new(), Sha256::new()),
            || CaptureError::SourceChangedDuringCapture,
        )
        .unwrap();

        let first = stream.next_record().unwrap().unwrap();
        assert_eq!(stream.record_bytes(first), b"one");
        let after_first = stream.position();
        let second = stream.next_record().unwrap().unwrap();
        assert_eq!(stream.record_bytes(second), b"two");
        stream.restore(after_first).unwrap();
        assert_eq!(stream.next_record().unwrap().unwrap(), second);
        let tail = stream.next_record().unwrap().unwrap();
        assert!(!tail.complete);
        assert_eq!(stream.record_bytes(tail), b"incomplete");
        let after_tail = stream.position();
        assert!(stream.next_record().unwrap().is_none());
        stream.restore(after_tail).unwrap();
        assert!(stream.next_record().unwrap().is_none());
        assert_eq!(stream.complete_prefix_end(), 8);
        assert_eq!(stream.next_physical_ordinal(), 2);
        assert!(!stream.terminal());
        let digest = stream.digest();
        let complete: [u8; 32] = digest.complete_hasher().clone().finalize().into();
        let expected_complete: [u8; 32] = Sha256::digest(&contents[..8]).into();
        assert_eq!(complete, expected_complete);
        let full: [u8; 32] = digest.full_hasher().unwrap().clone().finalize().into();
        let expected_full: [u8; 32] = Sha256::digest(contents).into();
        assert_eq!(full, expected_full);
    }
}
