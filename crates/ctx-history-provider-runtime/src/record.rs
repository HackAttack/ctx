use ctx_history_capture_model::RecordDigest;
use ctx_history_source_sqlite::NativeSqliteValue;

/// Stable persisted digest of one provider-native logical SQLite row.
pub fn sqlite_logical_record_digest(values: &[NativeSqliteValue]) -> RecordDigest {
    RecordDigest::from_sha256(ctx_history_source_sqlite::sqlite_logical_record_digest_bytes(values))
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
