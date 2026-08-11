pub(crate) mod adapter;
pub(crate) mod codex;
pub(crate) mod ctx_retrieval;
pub(crate) mod custom_history_jsonl;
pub(crate) mod file_touches;
pub(crate) mod native_ingestion;
pub(crate) mod normalization;
pub(crate) mod providers;
pub mod source_backed;
pub(crate) mod sqlite;
pub(crate) mod tool_input;

const MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES: usize = 7 * 1024;

pub(crate) fn provider_path_identity(path: &std::path::Path) -> crate::Result<String> {
    if path.to_str().is_none() {
        return Err(crate::CaptureError::InvalidProviderTranscriptPath {
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
        return Err(crate::CaptureError::InvalidProviderTranscriptPath {
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

pub(crate) use ctx_history_source_io::provider_safe_path_segment;
