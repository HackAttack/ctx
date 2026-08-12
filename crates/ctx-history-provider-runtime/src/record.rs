use ctx_history_capture_model::RecordDigest;
use ctx_history_source_sqlite::NativeSqliteValue;
use sha2::{Digest, Sha256};

/// Stable persisted digest of one provider-native logical SQLite row.
pub fn sqlite_logical_record_digest(values: &[NativeSqliteValue]) -> RecordDigest {
    // This domain is persisted evidence. Keep its released bytes stable even
    // though the resolver architecture that originally named it is gone.
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
    RecordDigest::from_sha256(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_digest_bytes_remain_stable() {
        let digest = sqlite_logical_record_digest(&[
            NativeSqliteValue::Null,
            NativeSqliteValue::Integer(-7),
            NativeSqliteValue::from_real(1.5),
            NativeSqliteValue::Text("ctx".to_owned()),
            NativeSqliteValue::Blob(vec![0, 1, 2]),
        ]);
        assert_eq!(
            digest.as_str(),
            "4473490dad215e0412d68454c71a82a60bc69608cc469b931e11625f03f3e432"
        );
    }
}
