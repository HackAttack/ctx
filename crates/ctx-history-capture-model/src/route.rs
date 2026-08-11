use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Exact identity of one selected ingestion route.
///
/// The digest is derived by discovery from the provider, format, selection
/// authority, and exact local route locator; paths themselves do not enter
/// Core or Pro records. Deserialization remains transparent and deliberately
/// defers validation so persisted corruption reaches the owning format layer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRouteIdentity(String);

/// A source-route identity was not exactly one lowercase SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("source route identity is not exactly 64 lowercase hexadecimal characters")]
pub struct SourceRouteIdentityError;

impl SourceRouteIdentity {
    pub fn from_sha256(value: String) -> Result<Self, SourceRouteIdentityError> {
        let identity = Self(value);
        identity.validate()?;
        Ok(identity)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validates the persisted route identity after deserialization.
    pub fn validate(&self) -> Result<(), SourceRouteIdentityError> {
        if is_lowercase_sha256(&self.0) {
            Ok(())
        } else {
            Err(SourceRouteIdentityError)
        }
    }
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_identity_preserves_transparent_wire_form_and_validates_sha256() {
        let value = "ab".repeat(32);
        let identity = SourceRouteIdentity::from_sha256(value.clone()).unwrap();

        assert_eq!(identity.as_str(), value);
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            format!("\"{value}\"")
        );
        assert!(identity.validate().is_ok());
        assert_eq!(
            SourceRouteIdentity::from_sha256("AB".repeat(32)),
            Err(SourceRouteIdentityError)
        );
        assert_eq!(
            SourceRouteIdentity::from_sha256("a".repeat(63)),
            Err(SourceRouteIdentityError)
        );
    }

    #[test]
    fn route_identity_deserialization_defers_validation() {
        let malformed: SourceRouteIdentity =
            serde_json::from_str(&format!("\"{}\"", "AB".repeat(32))).unwrap();

        assert_eq!(malformed.as_str(), "AB".repeat(32));
        assert_eq!(malformed.validate(), Err(SourceRouteIdentityError));
    }
}
