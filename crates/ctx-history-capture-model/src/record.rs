use std::fmt;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

/// Canonical SHA-256 evidence for one provider-native logical record.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RecordDigest(String);

impl RecordDigest {
    pub fn from_text(text: &str) -> Self {
        Self::from_bytes(text.as_bytes())
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RecordDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).ok_or_else(|| D::Error::custom("expected lowercase SHA-256 hex"))
    }
}

impl fmt::Debug for RecordDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RecordDigest(<sha256>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_digest_serde_and_redacted_debug_are_stable() {
        let digest = RecordDigest::from_text("capture record");
        assert_eq!(
            digest.as_str(),
            "6beaac033d1794b05306aa208661a73e206ac60fc2ea6b207fd569dee325256a"
        );
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(
            json,
            r#""6beaac033d1794b05306aa208661a73e206ac60fc2ea6b207fd569dee325256a""#
        );
        assert_eq!(serde_json::from_str::<RecordDigest>(&json).unwrap(), digest);
        assert_eq!(format!("{digest:?}"), "RecordDigest(<sha256>)");
    }

    #[test]
    fn record_digest_rejects_noncanonical_text() {
        assert!(RecordDigest::parse("A".repeat(64)).is_none());
        assert!(RecordDigest::parse("0".repeat(63)).is_none());
        assert!(serde_json::from_str::<RecordDigest>(&format!("\"{}\"", "g".repeat(64))).is_err());
    }
}
