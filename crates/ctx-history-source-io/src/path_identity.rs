use std::path::Path;

use crate::{Result, SourceIoError};

const MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES: usize = 7 * 1024;

/// Encodes one native provider path as its stable durable TEXT identity.
pub fn provider_path_identity(path: &Path) -> Result<String> {
    if path.to_str().is_none() {
        return Err(SourceIoError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason:
                "provider transcript path is not Unicode and cannot share durable TEXT authority",
        });
    }
    #[cfg(unix)]
    let (platform, raw) = {
        use std::os::unix::ffi::OsStrExt;

        ("unix-bytes", path.as_os_str().as_bytes().to_vec())
    };
    #[cfg(windows)]
    let (platform, raw) = {
        use std::os::windows::ffi::OsStrExt;

        let mut raw = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            raw.extend_from_slice(&unit.to_le_bytes());
        }
        ("windows-wtf16le", raw)
    };
    #[cfg(not(any(unix, windows)))]
    let (platform, raw) = (
        "platform-encoded-bytes",
        path.as_os_str().as_encoded_bytes().to_vec(),
    );

    if raw.len() > MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES {
        return Err(SourceIoError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript path exceeds the durable identity limit",
        });
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(
        "provider-path-v1:"
            .len()
            .saturating_add(platform.len())
            .saturating_add(1)
            .saturating_add(raw.len().saturating_mul(2)),
    );
    encoded.push_str("provider-path-v1:");
    encoded.push_str(platform);
    encoded.push(':');
    for byte in raw {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_path_identity_preserves_exact_native_bytes() {
        assert_eq!(
            provider_path_identity(Path::new("/tmp/ctx.jsonl")).unwrap(),
            "provider-path-v1:unix-bytes:2f746d702f6374782e6a736f6e6c"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_path_identity_keeps_the_existing_diagnostic() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"invalid-\xff"));
        let error = provider_path_identity(path).unwrap_err();
        assert!(matches!(
            error,
            SourceIoError::InvalidProviderTranscriptPath {
                reason: "provider transcript path is not Unicode and cannot share durable TEXT authority",
                ..
            }
        ));
    }

    #[test]
    fn path_identity_rejects_raw_paths_above_the_durable_limit() {
        let path = Path::new(&"x".repeat(MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES + 1)).to_path_buf();
        let error = provider_path_identity(&path).unwrap_err();
        assert!(matches!(
            error,
            SourceIoError::InvalidProviderTranscriptPath {
                reason: "provider transcript path exceeds the durable identity limit",
                ..
            }
        ));
    }
}
