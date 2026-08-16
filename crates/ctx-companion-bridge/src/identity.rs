use std::{fmt, path::Path};

use crate::BridgeError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, BridgeError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(BridgeError::Verification(
                "SHA-256 identity is not lowercase hexadecimal".to_owned(),
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(chunk[0]);
            let low = hex_nibble(chunk[1]);
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        format!("{self}")
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    size_bytes: u64,
    sha256: Sha256Digest,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    owner: u32,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl FileIdentity {
    #[cfg(unix)]
    pub(crate) const fn unix(
        size_bytes: u64,
        sha256: Sha256Digest,
        device: u64,
        inode: u64,
        owner: u32,
    ) -> Self {
        Self {
            size_bytes,
            sha256,
            device,
            inode,
            owner,
        }
    }

    #[cfg(windows)]
    pub(crate) const fn windows(
        size_bytes: u64,
        sha256: Sha256Digest,
        volume_serial: u32,
        file_index: u64,
    ) -> Self {
        Self {
            size_bytes,
            sha256,
            volume_serial,
            file_index,
        }
    }

    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub const fn sha256(&self) -> Sha256Digest {
        self.sha256
    }

    #[cfg(unix)]
    pub const fn owner(&self) -> u32 {
        self.owner
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairIdentity {
    root: std::path::PathBuf,
    launcher: FileIdentity,
    companion: FileIdentity,
}

impl PairIdentity {
    pub(crate) fn new(
        root: std::path::PathBuf,
        launcher: FileIdentity,
        companion: FileIdentity,
    ) -> Self {
        Self {
            root,
            launcher,
            companion,
        }
    }

    pub fn managed_root(&self) -> &Path {
        &self.root
    }

    pub const fn launcher(&self) -> &FileIdentity {
        &self.launcher
    }

    pub const fn companion(&self) -> &FileIdentity {
        &self.companion
    }
}
