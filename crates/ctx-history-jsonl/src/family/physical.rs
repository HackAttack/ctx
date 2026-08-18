use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Seek, SeekFrom, Write},
};

use sha2::{Digest, Sha256};

use super::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_complete_sha256, read_bounded_record_full_complete_and_prefix_sha256,
    read_bounded_record_unhashed, JsonlFamilyError, JsonlRecordFraming, JsonlResult,
    JsonlResumableSha256,
};

const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
// Upstream writes one append batch per frame, so a frame may contain several
// individually bounded JSONL rows. Keep frame allocation bounded separately
// from the per-row parser limit.
const MAX_ZSTD_DECODED_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_ZSTD_FRAME_BYTES: usize = MAX_ZSTD_DECODED_FRAME_BYTES + (1024 * 1024);
const MAX_ZSTD_WINDOW_LOG: u32 = 26;

/// Hard physical bound for one provider-owned standard Zstandard JSONL stream.
pub const MAX_STANDARD_ZSTD_COMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Hard logical bound for one decoded standard Zstandard JSONL stream.
pub const MAX_STANDARD_ZSTD_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_STANDARD_ZSTD_EXPANSION_RATIO: u64 = 256;
const STANDARD_ZSTD_EXPANSION_SLACK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STANDARD_ZSTD_WINDOW_LOG: u32 = 27;
const STANDARD_ZSTD_COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlPhysicalEncoding {
    RawJsonl,
    ChecksummedZstdFrames,
    /// One ordinary Zstandard stream whose decoded bytes are JSONL records.
    StandardZstdJsonl,
}

impl JsonlPhysicalEncoding {
    pub const fn checkpoint_tag(self) -> &'static str {
        match self {
            Self::RawJsonl => "raw-jsonl-v1",
            Self::ChecksummedZstdFrames => "checksummed-zstd-frames-v1",
            Self::StandardZstdJsonl => "standard-zstd-jsonl-v1",
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
    logical_offset: u64,
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
    standard_zstd: Option<StandardZstdBacking>,
    frozen_length: u64,
    offset: u64,
    logical_offset: u64,
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

#[derive(Debug)]
struct StandardZstdBacking {
    physical_reader: BufReader<File>,
    logical_length: u64,
    total_records: u64,
}

#[derive(Debug, Clone, Copy)]
struct StandardZstdLimits {
    compressed_bytes: u64,
    decompressed_bytes: u64,
    expansion_ratio: u64,
    expansion_slack_bytes: u64,
}

impl StandardZstdLimits {
    const PRODUCTION: Self = Self {
        compressed_bytes: MAX_STANDARD_ZSTD_COMPRESSED_BYTES,
        decompressed_bytes: MAX_STANDARD_ZSTD_DECOMPRESSED_BYTES,
        expansion_ratio: MAX_STANDARD_ZSTD_EXPANSION_RATIO,
        expansion_slack_bytes: STANDARD_ZSTD_EXPANSION_SLACK_BYTES,
    };
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
        file: File,
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
        let (reader, standard_zstd, logical_offset) = match encoding {
            JsonlPhysicalEncoding::StandardZstdJsonl => {
                if offset != 0 || next_physical_ordinal != 0 {
                    return Err(E::invalid_payload(
                        "standard Zstandard JSONL does not support physical suffix resume"
                            .to_owned(),
                    ));
                }
                let decoded = decode_standard_zstd_jsonl::<E>(
                    file,
                    frozen_length,
                    framing,
                    StandardZstdLimits::PRODUCTION,
                )?;
                (
                    BufReader::new(decoded.plaintext),
                    Some(StandardZstdBacking {
                        physical_reader: decoded.physical_reader,
                        logical_length: decoded.logical_length,
                        total_records: decoded.total_records,
                    }),
                    0,
                )
            }
            JsonlPhysicalEncoding::RawJsonl | JsonlPhysicalEncoding::ChecksummedZstdFrames => {
                let mut file = file;
                file.seek(SeekFrom::Start(offset))?;
                (BufReader::new(file), None, offset)
            }
        };
        Ok(Self {
            reader,
            standard_zstd,
            frozen_length,
            offset,
            logical_offset,
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
        if self.stream_exhausted() {
            self.exhausted = true;
            return Ok(None);
        }
        let record = if self.encoding == JsonlPhysicalEncoding::StandardZstdJsonl {
            self.read_standard_zstd_record()?
        } else if self.encoding == JsonlPhysicalEncoding::ChecksummedZstdFrames {
            let remaining = self.frozen_length.saturating_sub(self.offset);
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
                byte_end_exclusive: None,
                sha256: Sha256::digest(&frame.physical_bytes).into(),
            }
        } else {
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
            JsonlDecodedPhysicalUnit {
                complete: record.complete,
                terminal_nul_padding: record.terminal_nul_padding,
                oversized: record.oversized,
                stored_len: record.stored_len,
                byte_len: record.byte_len,
                byte_end_exclusive: None,
                sha256: record.sha256,
            }
        };
        let byte_start = self.offset;
        let byte_end_exclusive = record.byte_end_exclusive.unwrap_or(
            byte_start
                .checked_add(record.byte_len)
                .ok_or_else(|| E::system_invariant("JSONL physical stream offset overflowed"))?,
        );
        self.offset = byte_end_exclusive;
        if self.encoding != JsonlPhysicalEncoding::StandardZstdJsonl {
            self.logical_offset = byte_end_exclusive;
        }
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
            logical_offset: self.logical_offset,
            next_physical_ordinal: self.next_physical_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            digest: self.digest.clone(),
            incomplete_tail: self.incomplete_tail,
            exhausted: self.exhausted,
        }
    }

    pub fn restore(&mut self, position: JsonlPhysicalStreamPosition) -> JsonlResult<(), E> {
        self.reader.seek(SeekFrom::Start(position.logical_offset))?;
        if let Some(standard_zstd) = self.standard_zstd.as_mut() {
            standard_zstd
                .physical_reader
                .seek(SeekFrom::Start(position.offset))?;
        }
        self.offset = position.offset;
        self.logical_offset = position.logical_offset;
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

    fn stream_exhausted(&self) -> bool {
        self.standard_zstd.as_ref().map_or_else(
            || self.offset == self.frozen_length,
            |standard_zstd| self.logical_offset == standard_zstd.logical_length,
        )
    }

    fn read_standard_zstd_record(&mut self) -> JsonlResult<JsonlDecodedPhysicalUnit, E> {
        let standard_zstd = self.standard_zstd.as_mut().ok_or_else(|| {
            E::system_invariant("standard Zstandard JSONL stream lost its decoded backing")
        })?;
        let logical_remaining = standard_zstd
            .logical_length
            .saturating_sub(self.logical_offset);
        let decoded = read_bounded_record_unhashed::<E>(
            &mut self.reader,
            &mut self.record_buffer,
            logical_remaining,
            self.framing,
            self.source_changed,
        )?
        .ok_or_else(|| (self.source_changed)())?;
        let logical_end = self
            .logical_offset
            .checked_add(decoded.byte_len)
            .ok_or_else(|| E::system_invariant("decoded Zstandard JSONL offset overflowed"))?;
        let next_ordinal = self
            .next_physical_ordinal
            .checked_add(u64::from(decoded.complete))
            .ok_or_else(|| E::system_invariant("decoded Zstandard JSONL ordinal overflowed"))?;
        let physical_end = if decoded.complete {
            standard_zstd_physical_end(
                self.offset,
                self.frozen_length,
                logical_end,
                standard_zstd.logical_length,
                next_ordinal,
                standard_zstd.total_records,
            )
        } else {
            self.offset
        };
        advance_standard_zstd_digest::<E>(
            &mut standard_zstd.physical_reader,
            &mut self.digest,
            self.offset,
            physical_end,
            decoded.complete,
        )?;
        self.logical_offset = logical_end;
        let sha256 = if decoded.terminal_nul_padding {
            [0; 32]
        } else {
            Sha256::digest(&self.record_buffer[..decoded.stored_len]).into()
        };
        Ok(JsonlDecodedPhysicalUnit {
            complete: decoded.complete,
            terminal_nul_padding: decoded.terminal_nul_padding,
            oversized: decoded.oversized,
            stored_len: decoded.stored_len,
            byte_len: physical_end.saturating_sub(self.offset),
            byte_end_exclusive: Some(physical_end),
            sha256,
        })
    }
}

struct DecodedStandardZstd {
    plaintext: File,
    physical_reader: BufReader<File>,
    logical_length: u64,
    total_records: u64,
}

fn decode_standard_zstd_jsonl<E: JsonlFamilyError>(
    file: File,
    compressed_length: u64,
    framing: JsonlRecordFraming,
    limits: StandardZstdLimits,
) -> JsonlResult<DecodedStandardZstd, E> {
    if compressed_length == 0 {
        return Err(invalid_standard_zstd::<E>("compressed stream is empty"));
    }
    if compressed_length > limits.compressed_bytes {
        return Err(invalid_standard_zstd::<E>(
            "compressed stream exceeds the bounded physical limit",
        ));
    }
    let ratio_bound = compressed_length
        .saturating_mul(limits.expansion_ratio)
        .saturating_add(limits.expansion_slack_bytes);
    let decoded_bound = limits.decompressed_bytes.min(ratio_bound);
    let mut decoder = zstd::stream::read::Decoder::new(file).map_err(|error| {
        E::invalid_payload(format!(
            "invalid standard Zstandard JSONL stream header: {error}"
        ))
    })?;
    decoder.window_log_max(MAX_STANDARD_ZSTD_WINDOW_LOG)?;
    let mut plaintext = tempfile::tempfile()?;
    let mut buffer = [0_u8; STANDARD_ZSTD_COPY_BUFFER_BYTES];
    let mut logical_length = 0_u64;
    let mut total_records = 0_u64;
    let mut trailing_bytes = 0_u64;
    let mut trailing_bytes_are_nul = true;
    loop {
        let read = decoder.read(&mut buffer).map_err(|error| {
            E::invalid_payload(format!(
                "corrupt or truncated Zstandard JSONL stream: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        let read_u64 = u64::try_from(read)
            .map_err(|_| E::system_invariant("decoded Zstandard chunk exceeds u64"))?;
        logical_length = logical_length.checked_add(read_u64).ok_or_else(|| {
            E::invalid_payload("decoded Zstandard JSONL length overflowed".to_owned())
        })?;
        if logical_length > decoded_bound {
            return Err(invalid_standard_zstd::<E>(
                "decoded stream exceeds the bounded decompression limit",
            ));
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                total_records = total_records.checked_add(1).ok_or_else(|| {
                    E::invalid_payload("decoded Zstandard JSONL record count overflowed".to_owned())
                })?;
                trailing_bytes = 0;
                trailing_bytes_are_nul = true;
            } else {
                trailing_bytes = trailing_bytes.saturating_add(1);
                trailing_bytes_are_nul &= *byte == 0;
            }
        }
        plaintext.write_all(&buffer[..read])?;
    }
    let mut physical_reader = decoder.finish();
    if logical_length == 0 {
        return Err(invalid_standard_zstd::<E>(
            "decoded stream contains no JSONL records",
        ));
    }
    if trailing_bytes != 0 {
        if framing.allows_terminal_nul_padding() && trailing_bytes_are_nul {
            total_records = total_records.checked_add(1).ok_or_else(|| {
                E::invalid_payload("decoded Zstandard JSONL record count overflowed".to_owned())
            })?;
        } else {
            return Err(invalid_standard_zstd::<E>(
                "decoded stream has a non-terminated JSONL tail",
            ));
        }
    }
    if total_records == 0 {
        return Err(invalid_standard_zstd::<E>(
            "decoded stream contains no complete JSONL records",
        ));
    }
    plaintext.flush()?;
    plaintext.seek(SeekFrom::Start(0))?;
    physical_reader.seek(SeekFrom::Start(0))?;
    Ok(DecodedStandardZstd {
        plaintext,
        physical_reader,
        logical_length,
        total_records,
    })
}

fn standard_zstd_physical_end(
    current: u64,
    physical_length: u64,
    logical_end: u64,
    logical_length: u64,
    next_ordinal: u64,
    total_records: u64,
) -> u64 {
    if logical_end >= logical_length || next_ordinal >= total_records {
        return physical_length;
    }
    let proportional =
        (u128::from(logical_end) * u128::from(physical_length) / u128::from(logical_length)) as u64;
    current
        .max(proportional)
        .min(physical_length.saturating_sub(1))
}

fn advance_standard_zstd_digest<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    digest: &mut JsonlPhysicalDigest,
    start: u64,
    end: u64,
    complete: bool,
) -> JsonlResult<(), E> {
    if end < start {
        return Err(E::system_invariant(
            "standard Zstandard JSONL physical progress regressed",
        ));
    }
    let mut remaining = end - start;
    let mut buffer = [0_u8; STANDARD_ZSTD_COPY_BUFFER_BYTES];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| E::system_invariant("Zstandard digest chunk exceeds usize"))?;
        reader.read_exact(&mut buffer[..take])?;
        digest.update_physical_unit(&buffer[..take], complete);
        remaining = remaining.saturating_sub(take as u64);
    }
    Ok(())
}

fn invalid_standard_zstd<E: JsonlFamilyError>(detail: &str) -> E {
    E::invalid_payload(format!("invalid standard Zstandard JSONL: {detail}"))
}

struct JsonlDecodedPhysicalUnit {
    complete: bool,
    terminal_nul_padding: bool,
    oversized: bool,
    stored_len: usize,
    byte_len: u64,
    byte_end_exclusive: Option<u64>,
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

    fn standard_stream(plaintext: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(Cursor::new(plaintext), 1).unwrap()
    }

    #[test]
    fn standard_zstd_jsonl_streams_bounded_records_and_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl.zst");
        let plaintext = b"{\"type\":\"session_meta\"}\n{\"type\":\"response_item\"}\n";
        let compressed = standard_stream(plaintext);
        std::fs::write(&path, &compressed).unwrap();
        let mut stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&path).unwrap(),
            compressed.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::StandardZstdJsonl,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::full_and_complete(
                JsonlResumableSha256::new(),
                JsonlResumableSha256::new(),
            ),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();

        let first = stream.next_record().unwrap().unwrap();
        assert_eq!(stream.record_bytes(first), b"{\"type\":\"session_meta\"}");
        let after_first = stream.position();
        let second = stream.next_record().unwrap().unwrap();
        assert_eq!(stream.record_bytes(second), b"{\"type\":\"response_item\"}");
        stream.restore(after_first).unwrap();
        assert_eq!(stream.next_record().unwrap().unwrap(), second);
        assert!(stream.next_record().unwrap().is_none());
        assert!(stream.terminal());
        assert_eq!(stream.complete_prefix_end(), compressed.len() as u64);
        assert_eq!(stream.next_physical_ordinal(), 2);
        assert_eq!(
            stream.digest().complete_hasher().digest(),
            <[u8; 32]>::from(Sha256::digest(&compressed))
        );
        assert_eq!(
            stream.digest().full_hasher().unwrap().digest(),
            <[u8; 32]>::from(Sha256::digest(&compressed))
        );
    }

    #[test]
    fn standard_zstd_jsonl_rejects_corrupt_truncated_and_unterminated_streams() {
        let temp = tempfile::tempdir().unwrap();
        let valid = standard_stream(b"{}\n");
        let cases = [
            ("corrupt", {
                let mut value = valid.clone();
                value[0] ^= 0xff;
                value
            }),
            ("truncated", valid[..valid.len().saturating_sub(2)].to_vec()),
            ("unterminated", standard_stream(b"{}")),
        ];
        for (label, bytes) in cases {
            let path = temp.path().join(format!("{label}.jsonl.zst"));
            std::fs::write(&path, &bytes).unwrap();
            let result = JsonlPhysicalStream::open_with_encoding(
                File::open(&path).unwrap(),
                bytes.len() as u64,
                0,
                0,
                JsonlPhysicalEncoding::StandardZstdJsonl,
                JsonlRecordFraming::ordinary(),
                JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
                || SourceIoError::SourceChangedDuringCapture,
            );
            assert!(result.is_err(), "{label} stream was accepted");
        }
    }

    #[test]
    fn standard_zstd_jsonl_enforces_physical_logical_and_expansion_limits() {
        let temp = tempfile::tempdir().unwrap();
        let plaintext = b"{}\n".repeat(4096);
        let compressed = standard_stream(&plaintext);
        let path = temp.path().join("bounded.jsonl.zst");
        std::fs::write(&path, &compressed).unwrap();
        let base = StandardZstdLimits {
            compressed_bytes: compressed.len() as u64,
            decompressed_bytes: plaintext.len() as u64,
            expansion_ratio: u64::MAX,
            expansion_slack_bytes: u64::MAX,
        };

        for limits in [
            StandardZstdLimits {
                compressed_bytes: (compressed.len() as u64).saturating_sub(1),
                ..base
            },
            StandardZstdLimits {
                decompressed_bytes: (plaintext.len() as u64).saturating_sub(1),
                ..base
            },
            StandardZstdLimits {
                expansion_ratio: 1,
                expansion_slack_bytes: 0,
                ..base
            },
        ] {
            let result = decode_standard_zstd_jsonl::<SourceIoError>(
                File::open(&path).unwrap(),
                compressed.len() as u64,
                JsonlRecordFraming::ordinary(),
                limits,
            );
            assert!(result.is_err());
        }
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
