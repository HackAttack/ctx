use std::path::Path;

use crate::{Result, SourceIoError};

const MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES: usize = 7 * 1024;

/// Encodes one native provider path as its stable durable TEXT identity.
pub fn provider_path_identity(path: &Path) -> Result<String> {
    ensure_provider_path_identity_unicode(path, path.to_str().is_some())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        return provider_path_identity_from_raw(
            path,
            "unix-bytes",
            path.as_os_str().as_bytes().to_vec(),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        return windows_provider_path_identity(path, true, path.as_os_str().encode_wide());
    }
    #[cfg(not(any(unix, windows)))]
    {
        provider_path_identity_from_raw(
            path,
            "platform-encoded-bytes",
            path.as_os_str().as_encoded_bytes().to_vec(),
        )
    }
}

#[cfg(any(windows, test))]
fn windows_wtf16le_bytes(units: impl IntoIterator<Item = u16>) -> Vec<u8> {
    units.into_iter().flat_map(u16::to_le_bytes).collect()
}

#[cfg(any(windows, test))]
fn windows_provider_path_identity(
    path: &Path,
    unicode: bool,
    units: impl IntoIterator<Item = u16>,
) -> Result<String> {
    ensure_provider_path_identity_unicode(path, unicode)?;
    provider_path_identity_from_raw(path, "windows-wtf16le", windows_wtf16le_bytes(units))
}

fn ensure_provider_path_identity_unicode(path: &Path, unicode: bool) -> Result<()> {
    if unicode {
        return Ok(());
    }
    Err(SourceIoError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "provider transcript path is not Unicode and cannot share durable TEXT authority",
    })
}

fn provider_path_identity_from_raw(path: &Path, platform: &str, raw: Vec<u8>) -> Result<String> {
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

    #[test]
    fn windows_identity_is_exact_wtf16_little_endian() {
        assert_eq!(
            windows_provider_path_identity(
                Path::new("windows-test-path"),
                true,
                [
                    b'C' as u16,
                    b':' as u16,
                    b'\\' as u16,
                    0xd83d,
                    0xde80,
                    b'.' as u16,
                    b'j' as u16,
                ],
            )
            .unwrap(),
            "provider-path-v1:windows-wtf16le:43003a005c003dd880de2e006a00"
        );
    }

    #[test]
    fn windows_non_unicode_identity_keeps_the_existing_diagnostic() {
        let error = windows_provider_path_identity(Path::new("windows-test-path"), false, [0xd800])
            .unwrap_err();

        assert!(matches!(
            error,
            SourceIoError::InvalidProviderTranscriptPath {
                reason: "provider transcript path is not Unicode and cannot share durable TEXT authority",
                ..
            }
        ));
    }

    #[test]
    fn windows_identity_rejects_wtf16_bytes_above_the_durable_limit() {
        let error = windows_provider_path_identity(
            Path::new("windows-test-path"),
            true,
            std::iter::repeat_n(b'x' as u16, MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES / 2 + 1),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SourceIoError::InvalidProviderTranscriptPath {
                reason: "provider transcript path exceeds the durable identity limit",
                ..
            }
        ));
    }

    #[test]
    fn windows_identity_accepts_the_exact_wtf16_byte_limit() {
        let identity = windows_provider_path_identity(
            Path::new("windows-test-path"),
            true,
            std::iter::repeat_n(b'x' as u16, MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES / 2),
        )
        .unwrap();

        assert_eq!(
            identity.len(),
            "provider-path-v1:windows-wtf16le:".len() + MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES * 2
        );
    }
}
