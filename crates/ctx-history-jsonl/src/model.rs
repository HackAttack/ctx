use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::JsonlSourceIdentity;

const JSONL_PREFIX_HASH_DOMAIN: &[u8] = b"ctx-direct-jsonl-nativepath-prefix-v1\0";

#[inline]
pub fn new_jsonl_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(JSONL_PREFIX_HASH_DOMAIN);
    hasher
}

#[inline]
pub fn jsonl_prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl JsonlObservedTime {
    pub fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlFileObservation {
    length: u64,
    modified: JsonlObservedTime,
    readonly: bool,
    stable_identity: Option<[u8; 32]>,
    change_identity: Option<[u8; 32]>,
}

impl JsonlFileObservation {
    pub fn new(
        length: u64,
        modified: SystemTime,
        readonly: bool,
        stable_identity: Option<[u8; 32]>,
        change_identity: Option<[u8; 32]>,
    ) -> Self {
        Self {
            length,
            modified: JsonlObservedTime::from_system_time(modified),
            readonly,
            stable_identity,
            change_identity,
        }
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn same_stable_file(&self, current: &Self) -> bool {
        match (self.stable_identity, current.stable_identity) {
            (Some(previous), Some(current)) => previous == current,
            _ => false,
        }
    }

    pub fn supports_exact_revalidation(&self) -> bool {
        self.stable_identity.is_some() && self.change_identity.is_some()
    }

    /// Whether `current` can still contain the exact frozen bytes represented
    /// by this observation. Content is not trusted until the caller separately
    /// verifies its certified prefix digest.
    pub fn admits_frozen_prefix_in(&self, current: &Self) -> bool {
        self == current
            || (current.length >= self.length
                && self.supports_exact_revalidation()
                && self.same_stable_file(current))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlCheckpoint {
    version: u32,
    identity: JsonlSourceIdentity,
    source_observation: JsonlFileObservation,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    next_physical_ordinal: u64,
    terminal: bool,
}

impl JsonlCheckpoint {
    const VERSION: u32 = 1;

    pub fn new(
        identity: JsonlSourceIdentity,
        source_observation: JsonlFileObservation,
        complete_prefix_end: u64,
        complete_prefix_sha256: [u8; 32],
        next_physical_ordinal: u64,
        terminal: bool,
    ) -> Self {
        Self {
            version: Self::VERSION,
            identity,
            source_observation,
            complete_prefix_end,
            complete_prefix_sha256,
            next_physical_ordinal,
            terminal,
        }
    }

    pub fn identity(&self) -> &JsonlSourceIdentity {
        &self.identity
    }

    pub fn source_observation(&self) -> &JsonlFileObservation {
        &self.source_observation
    }

    pub fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub fn complete_prefix_sha256(&self) -> &[u8; 32] {
        &self.complete_prefix_sha256
    }

    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }

    pub fn is_internally_consistent(&self) -> bool {
        let empty_prefix = self.complete_prefix_end == 0;
        let empty_prefix_is_exact = self.next_physical_ordinal == 0
            && self.complete_prefix_sha256 == jsonl_prefix_digest(&new_jsonl_prefix_hasher());
        let nonempty_prefix_is_possible = self.next_physical_ordinal > 0
            && self.next_physical_ordinal <= self.complete_prefix_end;
        self.version == Self::VERSION
            && self.complete_prefix_end <= self.source_observation.length
            && if empty_prefix {
                empty_prefix_is_exact
            } else {
                nonempty_prefix_is_possible
            }
            && (!self.terminal || self.complete_prefix_end == self.source_observation.length)
    }

    pub fn supports(&self, identity: &JsonlSourceIdentity) -> bool {
        self.is_internally_consistent() && self.identity == *identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlSourceChange {
    Cold,
    Unchanged,
    Append,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlOversizedRecordPolicy {
    RejectSource,
    RejectRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlRecordEvidence {
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_digest: [u8; 32],
}

impl JsonlRecordEvidence {
    #[inline]
    pub fn new(
        physical_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        record_digest: [u8; 32],
    ) -> Self {
        Self {
            physical_ordinal,
            byte_start,
            byte_end_exclusive,
            record_digest,
        }
    }

    #[inline]
    pub fn physical_ordinal(self) -> u64 {
        self.physical_ordinal
    }

    #[inline]
    pub fn byte_start(self) -> u64 {
        self.byte_start
    }

    #[inline]
    pub fn byte_end_exclusive(self) -> u64 {
        self.byte_end_exclusive
    }

    #[inline]
    pub fn record_digest(self) -> [u8; 32] {
        self.record_digest
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JsonlRecordRef<'record> {
    bytes: &'record [u8],
    evidence: JsonlRecordEvidence,
    oversized: bool,
}

impl<'record> JsonlRecordRef<'record> {
    #[inline]
    pub fn new(bytes: &'record [u8], evidence: JsonlRecordEvidence, oversized: bool) -> Self {
        Self {
            bytes,
            evidence,
            oversized,
        }
    }

    #[doc(hidden)]
    pub fn for_test(bytes: &'record [u8], physical_ordinal: u64) -> Self {
        Self::new(
            bytes,
            JsonlRecordEvidence::new(
                physical_ordinal,
                0,
                bytes.len() as u64,
                Sha256::digest(bytes).into(),
            ),
            false,
        )
    }

    #[inline]
    pub fn bytes(self) -> &'record [u8] {
        self.bytes
    }

    #[inline]
    pub fn evidence(self) -> JsonlRecordEvidence {
        self.evidence
    }

    #[inline]
    pub fn oversized(self) -> bool {
        self.oversized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlPage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlScanOutcome {
    checkpoint: JsonlCheckpoint,
}

impl JsonlScanOutcome {
    pub fn new(checkpoint: JsonlCheckpoint) -> Self {
        Self { checkpoint }
    }

    pub fn checkpoint(&self) -> &JsonlCheckpoint {
        &self.checkpoint
    }
}
