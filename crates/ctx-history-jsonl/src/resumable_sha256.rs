use serde::{Deserialize, Serialize};
use sha2::{compress256, digest::generic_array::GenericArray};

const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Stable wire representation of a resumable SHA-256 computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlSha256State {
    version: u32,
    state: [u32; 8],
    bytes_hashed: u64,
    buffer: Vec<u8>,
}

/// SHA-256 with an explicit, implementation-independent resumable state.
#[derive(Debug, Clone)]
pub struct JsonlResumableSha256 {
    state: [u32; 8],
    bytes_hashed: u64,
    buffer: Vec<u8>,
}

impl Default for JsonlResumableSha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonlResumableSha256 {
    const STATE_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            state: SHA256_INITIAL_STATE,
            bytes_hashed: 0,
            buffer: Vec::new(),
        }
    }

    pub fn restore(snapshot: &JsonlSha256State) -> Option<Self> {
        let buffered = u64::try_from(snapshot.buffer.len()).ok()?;
        (snapshot.version == Self::STATE_VERSION
            && snapshot.buffer.len() < 64
            && snapshot.bytes_hashed % 64 == buffered
            && snapshot.bytes_hashed <= u64::MAX / 8)
            .then(|| Self {
                state: snapshot.state,
                bytes_hashed: snapshot.bytes_hashed,
                buffer: snapshot.buffer.clone(),
            })
    }

    pub fn snapshot(&self) -> JsonlSha256State {
        JsonlSha256State {
            version: Self::STATE_VERSION,
            state: self.state,
            bytes_hashed: self.bytes_hashed,
            buffer: self.buffer.clone(),
        }
    }

    pub fn bytes_hashed(&self) -> u64 {
        self.bytes_hashed
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.bytes_hashed = self
            .bytes_hashed
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .expect("SHA-256 input length exceeds its 64-bit bit-length encoding");
        if !self.buffer.is_empty() {
            let take = (64 - self.buffer.len()).min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == 64 {
                let block: [u8; 64] = self.buffer.as_slice().try_into().expect("full SHA block");
                compress_block(&mut self.state, &block);
                self.buffer.clear();
            }
        }
        while bytes.len() >= 64 {
            let (block, remaining) = bytes.split_at(64);
            compress_block(&mut self.state, block.try_into().expect("split SHA block"));
            bytes = remaining;
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut state = self.state;
        let bit_length = self
            .bytes_hashed
            .checked_mul(8)
            .expect("SHA-256 input length exceeds its bit-length encoding");
        let mut tail = self.buffer.clone();
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bit_length.to_be_bytes());
        for block in tail.chunks_exact(64) {
            compress_block(&mut state, block.try_into().expect("padded SHA block"));
        }
        let mut digest = [0_u8; 32];
        for (encoded, word) in digest.chunks_exact_mut(4).zip(state) {
            encoded.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

fn compress_block(state: &mut [u32; 8], block: &[u8; 64]) {
    let block = GenericArray::clone_from_slice(block);
    compress256(state, std::slice::from_ref(&block));
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn resumable_sha256_matches_standard_vectors_and_every_resume_boundary() {
        for input in [
            &b""[..],
            &b"abc"[..],
            &b"a payload crossing the SHA-256 block boundary a payload crossing it again"[..],
        ] {
            let expected: [u8; 32] = Sha256::digest(input).into();
            for split in 0..=input.len() {
                let mut before = JsonlResumableSha256::new();
                before.update(&input[..split]);
                let mut resumed = JsonlResumableSha256::restore(&before.snapshot()).unwrap();
                resumed.update(&input[split..]);
                assert_eq!(resumed.digest(), expected, "split {split}");
            }
        }
    }
}
