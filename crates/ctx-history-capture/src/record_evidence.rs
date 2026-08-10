use sha2::{Digest, Sha256};

use crate::native_source::NativeSqliteValue;
pub(crate) use ctx_history_capture_model::RecordDigest;

pub(crate) fn sqlite_logical_record_digest(values: &[NativeSqliteValue]) -> RecordDigest {
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
    RecordDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}
