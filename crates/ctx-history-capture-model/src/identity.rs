use uuid::Uuid;

/// Derives the released deterministic capture identity for a dedupe key and role.
pub fn stable_capture_uuid(dedupe_key: &str, role: &str) -> Uuid {
    let mut bytes = [0_u8; 16];
    let name = format!("ctx-ctx-history-capture:{dedupe_key}:{role}");
    let first = fnv1a64(name.as_bytes()).to_be_bytes();
    let second = fnv1a64(format!("{name}:uuid-v7").as_bytes()).to_be_bytes();

    bytes[..6].copy_from_slice(&first[..6]);
    bytes[6] = 0x70 | (first[6] & 0x0f);
    bytes[7] = first[7];
    bytes[8] = 0x80 | (second[0] & 0x3f);
    bytes[9..].copy_from_slice(&second[1..]);
    Uuid::from_bytes(bytes)
}

/// Computes the released FNV-1a 64-bit value used by capture identity domains.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_capture_identity_bytes_are_stable() {
        assert_eq!(
            stable_capture_uuid("session:abc", "provider-session").to_string(),
            "c58c78c6-5d43-7d9a-8f61-a254c78f9409"
        );
        assert_eq!(fnv1a64(b"ctx-capture-model"), 0x01b4_0a64_0415_5cd5);
    }
}
