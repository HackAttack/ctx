use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Seek, SeekFrom},
};

use sha2::{Digest, Sha256};

use super::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_complete_sha256, read_bounded_record_full_complete_and_prefix_sha256,
    JsonlFamilyError, JsonlRecordFraming, JsonlResult, JsonlResumableSha256,
};

const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
// Upstream writes one append batch per frame, so a frame may contain several
// individually bounded JSONL rows. Keep frame allocation bounded separately
// from the per-row parser limit.
const MAX_ZSTD_DECODED_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_ZSTD_FRAME_BYTES: usize = MAX_ZSTD_DECODED_FRAME_BYTES + (1024 * 1024);
const MAX_ZSTD_WINDOW_LOG: u32 = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlPhysicalEncoding {
    RawJsonl,
    ChecksummedZstdFrames,
}

impl JsonlPhysicalEncoding {
    pub const fn checkpoint_tag(self) -> &'static str {
        match self {
            Self::RawJsonl => "raw-jsonl-v1",
            Self::ChecksummedZstdFrames => "checksummed-zstd-frames-v1",
        }
    }
}

#[derive(Debug, Clone)]
pub enum JsonlPhysicalDigest {
    Complete {
        complete: JsonlResumableSha256,
    },
    FullAndComplete {
        full: JsonlResumableSha256,
        complete: JsonlResumableSha256,
    },
    CompleteAndBoundedPrefix {
        complete: JsonlResumableSha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    },
    FullCompleteAndBoundedPrefix {
        full: JsonlResumableSha256,
        complete: JsonlResumableSha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    },
}

impl JsonlPhysicalDigest {
    pub fn complete(complete: JsonlResumableSha256) -> Self {
        Self::Complete { complete }
    }

    pub fn complete_and_bounded_prefix(
        complete: JsonlResumableSha256,
        bounded_prefix: Sha256,
        bounded_prefix_remaining: u64,
    ) -> Self {
        Self::CompleteAndBoundedPrefix {
            complete,
            bounded_prefix,
            bounded_prefix_remaining,
        }
    }

    pub fn full_and_complete(full: JsonlResumableSha256, complete: JsonlResumableSha256) -> Self {
        Self::FullAndComplete { full, complete }
    }

    pub fn full_complete_and_bounded_prefix(
        full: JsonlResumableSha256,
        complete: JsonlResumableSha256,
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

    pub fn complete_hasher(&self) -> &JsonlResumableSha256 {
        match self {
            Self::Complete { complete }
            | Self::FullAndComplete { complete, .. }
            | Self::CompleteAndBoundedPrefix { complete, .. }
            | Self::FullCompleteAndBoundedPrefix { complete, .. } => complete,
        }
    }

    pub fn full_hasher(&self) -> Option<&JsonlResumableSha256> {
        match self {
            Self::FullAndComplete { full, .. }
            | Self::FullCompleteAndBoundedPrefix { full, .. } => Some(full),
            _ => None,
        }
    }

    pub fn bounded_prefix(&self) -> Option<(&Sha256, u64)> {
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

    fn update_physical_unit(&mut self, bytes: &[u8], complete: bool) {
        match self {
            Self::Complete { complete: digest } => {
                if complete {
                    digest.update(bytes);
                }
            }
            Self::FullAndComplete {
                full,
                complete: digest,
            } => {
                full.update(bytes);
                if complete {
                    digest.update(bytes);
                }
            }
            Self::CompleteAndBoundedPrefix {
                complete: digest,
                bounded_prefix,
                bounded_prefix_remaining,
            } => {
                update_bounded_prefix(bounded_prefix, bounded_prefix_remaining, bytes);
                if complete {
                    digest.update(bytes);
                }
            }
            Self::FullCompleteAndBoundedPrefix {
                full,
                complete: digest,
                bounded_prefix,
                bounded_prefix_remaining,
            } => {
                full.update(bytes);
                update_bounded_prefix(bounded_prefix, bounded_prefix_remaining, bytes);
                if complete {
                    digest.update(bytes);
                }
            }
        }
    }
}

fn update_bounded_prefix(hasher: &mut Sha256, remaining: &mut u64, bytes: &[u8]) {
    let take = usize::try_from((*remaining).min(bytes.len() as u64)).unwrap_or(bytes.len());
    hasher.update(&bytes[..take]);
    *remaining = remaining.saturating_sub(u64::try_from(take).unwrap_or(u64::MAX));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlPhysicalRecord {
    pub physical_ordinal: u64,
    pub byte_start: u64,
    pub byte_end_exclusive: u64,
    pub complete: bool,
    pub terminal_nul_padding: bool,
    pub oversized: bool,
    pub stored_len: usize,
    pub sha256: [u8; 32],
}

impl JsonlPhysicalRecord {
    pub fn byte_len(self) -> u64 {
        self.byte_end_exclusive.saturating_sub(self.byte_start)
    }
}

#[derive(Debug, Clone)]
pub struct JsonlPhysicalStreamPosition {
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    digest: JsonlPhysicalDigest,
    incomplete_tail: bool,
    exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlPhysicalPassBinding {
    frozen_length: u64,
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    admitted_eof_sha256: Option<[u8; 32]>,
    incomplete_tail: bool,
    exhausted: bool,
}

impl JsonlPhysicalPassBinding {
    pub fn frozen_length(&self) -> u64 {
        self.frozen_length
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub fn complete_prefix_sha256(&self) -> &[u8; 32] {
        &self.complete_prefix_sha256
    }

    pub fn admitted_eof_sha256(&self) -> Option<&[u8; 32]> {
        self.admitted_eof_sha256.as_ref()
    }

    pub fn incomplete_tail(&self) -> bool {
        self.incomplete_tail
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted
    }
}

#[derive(Debug)]
pub struct JsonlPhysicalStream<E: JsonlFamilyError> {
    reader: BufReader<File>,
    frozen_length: u64,
    offset: u64,
    next_physical_ordinal: u64,
    complete_prefix_end: u64,
    encoding: JsonlPhysicalEncoding,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
    digest: JsonlPhysicalDigest,
    record_buffer: Vec<u8>,
    incomplete_tail: bool,
    exhausted: bool,
}

impl<E: JsonlFamilyError> JsonlPhysicalStream<E> {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        file: File,
        frozen_length: u64,
        offset: u64,
        next_physical_ordinal: u64,
        framing: JsonlRecordFraming,
        digest: JsonlPhysicalDigest,
        source_changed: fn() -> E,
    ) -> JsonlResult<Self, E> {
        Self::open_with_encoding(
            file,
            frozen_length,
            offset,
            next_physical_ordinal,
            JsonlPhysicalEncoding::RawJsonl,
            framing,
            digest,
            source_changed,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_with_encoding(
        mut file: File,
        frozen_length: u64,
        offset: u64,
        next_physical_ordinal: u64,
        encoding: JsonlPhysicalEncoding,
        framing: JsonlRecordFraming,
        digest: JsonlPhysicalDigest,
        source_changed: fn() -> E,
    ) -> JsonlResult<Self, E> {
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
            encoding,
            framing,
            source_changed,
            digest,
            record_buffer: Vec::new(),
            incomplete_tail: false,
            exhausted: false,
        })
    }

    pub fn next_record(&mut self) -> JsonlResult<Option<JsonlPhysicalRecord>, E> {
        if self.exhausted {
            return Ok(None);
        }
        if self.offset == self.frozen_length {
            self.exhausted = true;
            return Ok(None);
        }
        let remaining = self.frozen_length.saturating_sub(self.offset);
        let record = if self.encoding == JsonlPhysicalEncoding::ChecksummedZstdFrames {
            let frame = read_checksummed_zstd_frame::<E>(
                &mut self.reader,
                &mut self.record_buffer,
                remaining,
            )?;
            self.digest
                .update_physical_unit(&frame.physical_bytes, frame.complete);
            JsonlDecodedPhysicalUnit {
                complete: frame.complete,
                terminal_nul_padding: false,
                oversized: false,
                stored_len: self.record_buffer.len(),
                byte_len: u64::try_from(frame.physical_bytes.len())
                    .map_err(|_| E::system_invariant("Zstandard frame length exceeds u64"))?,
                sha256: Sha256::digest(&frame.physical_bytes).into(),
            }
        } else {
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
            JsonlDecodedPhysicalUnit {
                complete: record.complete,
                terminal_nul_padding: record.terminal_nul_padding,
                oversized: record.oversized,
                stored_len: record.stored_len,
                byte_len: record.byte_len,
                sha256: record.sha256,
            }
        };
        let byte_start = self.offset;
        let byte_end_exclusive = byte_start
            .checked_add(record.byte_len)
            .ok_or_else(|| E::system_invariant("JSONL physical stream offset overflowed"))?;
        self.offset = byte_end_exclusive;
        let physical_ordinal = self.next_physical_ordinal;
        if record.complete {
            self.complete_prefix_end = byte_end_exclusive;
            self.next_physical_ordinal = self
                .next_physical_ordinal
                .checked_add(1)
                .ok_or_else(|| E::system_invariant("JSONL physical stream ordinal overflowed"))?;
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

    pub fn record_bytes(&self, record: JsonlPhysicalRecord) -> &[u8] {
        &self.record_buffer[..record.stored_len]
    }

    /// Drops retained record capacity before a prepared page crosses a worker
    /// boundary. Providers that do not transfer pages keep the allocation.
    pub fn release_record_buffer(&mut self) {
        self.record_buffer = Vec::new();
    }

    pub fn position(&self) -> JsonlPhysicalStreamPosition {
        JsonlPhysicalStreamPosition {
            offset: self.offset,
            next_physical_ordinal: self.next_physical_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            digest: self.digest.clone(),
            incomplete_tail: self.incomplete_tail,
            exhausted: self.exhausted,
        }
    }

    pub fn restore(&mut self, position: JsonlPhysicalStreamPosition) -> JsonlResult<(), E> {
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.offset = position.offset;
        self.next_physical_ordinal = position.next_physical_ordinal;
        self.complete_prefix_end = position.complete_prefix_end;
        self.digest = position.digest;
        self.incomplete_tail = position.incomplete_tail;
        self.exhausted = position.exhausted;
        Ok(())
    }

    pub fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn digest(&self) -> &JsonlPhysicalDigest {
        &self.digest
    }

    pub fn admitted_pass_binding(&self) -> JsonlPhysicalPassBinding {
        JsonlPhysicalPassBinding {
            frozen_length: self.frozen_length,
            offset: self.offset,
            next_physical_ordinal: self.next_physical_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: self.digest.complete_hasher().digest(),
            admitted_eof_sha256: self.digest.full_hasher().map(JsonlResumableSha256::digest),
            incomplete_tail: self.incomplete_tail,
            exhausted: self.exhausted,
        }
    }

    pub fn terminal(&self) -> bool {
        self.exhausted && !self.incomplete_tail
    }
}

struct JsonlDecodedPhysicalUnit {
    complete: bool,
    terminal_nul_padding: bool,
    oversized: bool,
    stored_len: usize,
    byte_len: u64,
    sha256: [u8; 32],
}

struct ZstdFrameRead {
    physical_bytes: Vec<u8>,
    complete: bool,
}

fn read_checksummed_zstd_frame<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    plaintext: &mut Vec<u8>,
    maximum_bytes: u64,
) -> JsonlResult<ZstdFrameRead, E> {
    plaintext.clear();
    let mut physical = Vec::new();
    if !read_frame_part::<E>(reader, &mut physical, 4, maximum_bytes)? {
        return Ok(ZstdFrameRead {
            physical_bytes: physical,
            complete: false,
        });
    }
    if physical[..4] != ZSTD_FRAME_MAGIC {
        return Err(invalid_zstd::<E>("invalid frame magic"));
    }
    if !read_frame_part::<E>(reader, &mut physical, 1, maximum_bytes)? {
        return Ok(ZstdFrameRead {
            physical_bytes: physical,
            complete: false,
        });
    }
    let descriptor = physical[4];
    if descriptor & 0x18 != 0 {
        return Err(invalid_zstd::<E>("reserved frame-header bits are set"));
    }
    if descriptor & 0x04 == 0 {
        return Err(invalid_zstd::<E>("frame checksum is required"));
    }
    let single_segment = descriptor & 0x20 != 0;
    let dictionary_flag = descriptor & 0x03;
    let dictionary_bytes = if dictionary_flag == 3 {
        4
    } else {
        usize::from(dictionary_flag)
    };
    let content_size_flag = descriptor >> 6;
    let content_size_bytes = if content_size_flag == 0 {
        usize::from(single_segment)
    } else {
        1_usize << content_size_flag
    };
    let remaining_header = usize::from(!single_segment)
        .saturating_add(dictionary_bytes)
        .saturating_add(content_size_bytes);
    if !read_frame_part::<E>(reader, &mut physical, remaining_header, maximum_bytes)? {
        return Ok(ZstdFrameRead {
            physical_bytes: physical,
            complete: false,
        });
    }

    loop {
        let block_header_start = physical.len();
        if !read_frame_part::<E>(reader, &mut physical, 3, maximum_bytes)? {
            return Ok(ZstdFrameRead {
                physical_bytes: physical,
                complete: false,
            });
        }
        let block_header = u32::from(physical[block_header_start])
            | (u32::from(physical[block_header_start + 1]) << 8)
            | (u32::from(physical[block_header_start + 2]) << 16);
        let last_block = block_header & 1 != 0;
        let block_type = (block_header >> 1) & 0x03;
        if block_type == 0x03 {
            return Err(invalid_zstd::<E>("reserved block type is present"));
        }
        let block_size = usize::try_from(block_header >> 3)
            .map_err(|_| invalid_zstd::<E>("block size exceeds platform limits"))?;
        let payload_bytes = if block_type == 0x01 { 1 } else { block_size };
        if !read_frame_part::<E>(reader, &mut physical, payload_bytes, maximum_bytes)? {
            return Ok(ZstdFrameRead {
                physical_bytes: physical,
                complete: false,
            });
        }
        if last_block {
            break;
        }
    }
    if !read_frame_part::<E>(reader, &mut physical, 4, maximum_bytes)? {
        return Ok(ZstdFrameRead {
            physical_bytes: physical,
            complete: false,
        });
    }

    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(&physical))?.single_frame();
    decoder.window_log_max(MAX_ZSTD_WINDOW_LOG)?;
    decoder
        .take(u64::try_from(MAX_ZSTD_DECODED_FRAME_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(plaintext)
        .map_err(|error| {
            E::invalid_payload(format!(
                "corrupt checksummed Zstandard JSONL frame: {error}"
            ))
        })?;
    if plaintext.len() > MAX_ZSTD_DECODED_FRAME_BYTES {
        return Err(invalid_zstd::<E>(
            "decoded frame exceeds the bounded plaintext limit",
        ));
    }
    if plaintext.is_empty() || plaintext.last() != Some(&b'\n') {
        return Err(invalid_zstd::<E>(
            "complete frame does not contain newline-terminated JSONL",
        ));
    }
    Ok(ZstdFrameRead {
        physical_bytes: physical,
        complete: true,
    })
}

fn read_frame_part<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    frame: &mut Vec<u8>,
    requested: usize,
    maximum_bytes: u64,
) -> JsonlResult<bool, E> {
    if frame.len().saturating_add(requested) > MAX_ZSTD_FRAME_BYTES {
        return Err(invalid_zstd::<E>(
            "compressed frame exceeds the bounded frame limit",
        ));
    }
    let remaining = maximum_bytes.saturating_sub(frame.len() as u64);
    let available = usize::try_from(remaining.min(requested as u64))
        .map_err(|_| E::system_invariant("Zstandard frame bound exceeds usize"))?;
    let start = frame.len();
    frame.resize(start.saturating_add(available), 0);
    let mut filled = 0_usize;
    while filled < available {
        let read = reader.read(&mut frame[start + filled..start + available])?;
        if read == 0 {
            break;
        }
        filled = filled.saturating_add(read);
    }
    frame.truncate(start.saturating_add(filled));
    Ok(filled == requested)
}

fn invalid_zstd<E: JsonlFamilyError>(detail: &str) -> E {
    E::invalid_payload(format!("invalid checksummed Zstandard JSONL: {detail}"))
}

#[cfg(test)]
mod tests {
    use ctx_history_source_io::SourceIoError;
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn stream_tracks_complete_prefix_tail_and_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("records.jsonl");
        let contents = b"one\ntwo\nincomplete";
        std::fs::write(&path, contents).unwrap();
        let mut stream = JsonlPhysicalStream::open(
            File::open(&path).unwrap(),
            contents.len() as u64,
            0,
            0,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::full_and_complete(
                JsonlResumableSha256::new(),
                JsonlResumableSha256::new(),
            ),
            || SourceIoError::SourceChangedDuringCapture,
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
        let complete = digest.complete_hasher().digest();
        let expected_complete: [u8; 32] = Sha256::digest(&contents[..8]).into();
        assert_eq!(complete, expected_complete);
        let full = digest.full_hasher().unwrap().digest();
        let expected_full: [u8; 32] = Sha256::digest(contents).into();
        assert_eq!(full, expected_full);
    }

    fn checksummed_frame(plaintext: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        encoder.include_checksum(true).unwrap();
        std::io::Write::write_all(&mut encoder, plaintext).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn zstd_stream_decodes_concatenated_checksummed_frames_and_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl.zstd");
        let first_bytes = checksummed_frame(b"{\"type\":\"session\"}\n");
        let second_bytes =
            checksummed_frame(b"{\"type\":\"user/message\"}\n{\"type\":\"assistant/message\"}\n");
        let contents = [first_bytes.as_slice(), second_bytes.as_slice()].concat();
        std::fs::write(&path, &contents).unwrap();
        let mut stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&path).unwrap(),
            contents.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::ChecksummedZstdFrames,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::full_and_complete(
                JsonlResumableSha256::new(),
                JsonlResumableSha256::new(),
            ),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();

        let header = stream.next_record().unwrap().unwrap();
        assert_eq!(stream.record_bytes(header), b"{\"type\":\"session\"}\n");
        let after_header = stream.position();
        let batch = stream.next_record().unwrap().unwrap();
        assert_eq!(
            stream.record_bytes(batch),
            b"{\"type\":\"user/message\"}\n{\"type\":\"assistant/message\"}\n"
        );
        stream.restore(after_header).unwrap();
        assert_eq!(stream.next_record().unwrap().unwrap(), batch);
        assert!(stream.next_record().unwrap().is_none());
        assert!(stream.terminal());
        assert_eq!(stream.complete_prefix_end(), contents.len() as u64);
        assert_eq!(stream.next_physical_ordinal(), 2);
        assert_eq!(
            stream.digest().complete_hasher().digest(),
            <[u8; 32]>::from(Sha256::digest(&contents))
        );
    }

    #[test]
    fn zstd_stream_omits_torn_frame_and_rejects_corrupt_or_unchecked_frames() {
        let temp = tempfile::tempdir().unwrap();
        let complete = checksummed_frame(b"{\"type\":\"session\"}\n");
        let mut torn = checksummed_frame(b"{\"type\":\"user/message\"}\n");
        torn.truncate(torn.len().saturating_sub(2));
        let contents = [complete.as_slice(), torn.as_slice()].concat();
        let path = temp.path().join("torn.jsonl.zstd");
        std::fs::write(&path, &contents).unwrap();
        let mut stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&path).unwrap(),
            contents.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::ChecksummedZstdFrames,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::full_and_complete(
                JsonlResumableSha256::new(),
                JsonlResumableSha256::new(),
            ),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();
        assert!(stream.next_record().unwrap().unwrap().complete);
        assert!(!stream.next_record().unwrap().unwrap().complete);
        assert!(!stream.terminal());
        assert_eq!(stream.complete_prefix_end(), complete.len() as u64);
        assert_eq!(
            stream.digest().full_hasher().unwrap().digest(),
            <[u8; 32]>::from(Sha256::digest(&contents))
        );

        let mut corrupt = complete.clone();
        let last = corrupt.len().saturating_sub(1);
        corrupt[last] ^= 0xff;
        let corrupt_path = temp.path().join("corrupt.jsonl.zstd");
        std::fs::write(&corrupt_path, &corrupt).unwrap();
        let mut corrupt_stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&corrupt_path).unwrap(),
            corrupt.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::ChecksummedZstdFrames,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();
        assert!(corrupt_stream.next_record().is_err());

        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        std::io::Write::write_all(&mut encoder, b"{}\n").unwrap();
        let unchecked = encoder.finish().unwrap();
        let unchecked_path = temp.path().join("unchecked.jsonl.zstd");
        std::fs::write(&unchecked_path, &unchecked).unwrap();
        let mut unchecked_stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&unchecked_path).unwrap(),
            unchecked.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::ChecksummedZstdFrames,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();
        assert!(unchecked_stream.next_record().is_err());

        let mut reserved = complete.clone();
        reserved[4] |= 0x10;
        let reserved_path = temp.path().join("reserved.jsonl.zstd");
        std::fs::write(&reserved_path, &reserved).unwrap();
        let mut reserved_stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&reserved_path).unwrap(),
            reserved.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::ChecksummedZstdFrames,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();
        assert!(reserved_stream.next_record().is_err());
    }
}
