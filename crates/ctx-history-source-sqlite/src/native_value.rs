use crate::value::NativeSqliteValue;
use sha2::{Digest, Sha256};

/// Canonical SHA-256 bytes for the released logical-row evidence encoding.
pub fn sqlite_logical_record_digest_bytes(values: &[NativeSqliteValue]) -> [u8; 32] {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use crate::value::NativeSqliteValue;

    use super::*;

    #[test]
    fn released_logical_row_digest_bytes_remain_exact() {
        let digest = sqlite_logical_record_digest_bytes(&[
            NativeSqliteValue::Null,
            NativeSqliteValue::Integer(-7),
            NativeSqliteValue::from_real(1.5),
            NativeSqliteValue::Text("hé".to_owned()),
            NativeSqliteValue::Blob(vec![0, 255]),
        ]);
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            encoded,
            "c86454e26585325ec7d7880aabdd0f42eba4c5f63adf81e592fb4faa2df5f9d7"
        );
        assert_eq!(NativeSqliteValue::from_real(1.5).as_real(), Some(1.5));
    }
}
