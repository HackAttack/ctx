use std::path::PathBuf;

use ctx_history_core::CaptureProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryIssueKind {
    NoDiskHistory,
    SelectorUnreconstructible,
    InsufficientOfficialEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryIssue {
    pub provider: CaptureProvider,
    pub path: Option<PathBuf>,
    pub kind: DiscoveryIssueKind,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryReport {
    pub sources: Vec<ProviderSource>,
    pub issues: Vec<DiscoveryIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceKind {
    NativeHistory,
    DetectionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportSupport {
    Native,
    Explicit,
    Unsupported,
}

impl ProviderImportSupport {
    pub fn is_importable(self) -> bool {
        matches!(self, Self::Native | Self::Explicit)
    }

    pub fn is_auto_importable(self) -> bool {
        matches!(self, Self::Native)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCatalogSupport {
    Native,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatus {
    Available,
    Empty,
    Unknown,
    Missing,
    Unsupported,
}

impl ProviderSourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatusReason {
    BlockedAuthOrEncryption,
}

impl ProviderSourceStatusReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BlockedAuthOrEncryption => "blocked_auth_or_encryption",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefaultLocation {
    pub path_components: &'static [&'static str],
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSourceSpec {
    pub provider: CaptureProvider,
    pub display_name: &'static str,
    pub default_locations: &'static [ProviderDefaultLocation],
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub unsupported_reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSource {
    pub provider: CaptureProvider,
    pub path: PathBuf,
    pub exists: bool,
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub status: ProviderSourceStatus,
    pub unsupported_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceFailureKind {
    NotFound,
    Permission,
    Locked,
    Corrupt,
    SchemaIncompatible,
    InvalidSource,
    SourceChanged,
    SourceDatabase,
    Io,
}

impl std::fmt::Display for ProviderSourceFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::Locked => "locked",
            Self::Corrupt => "corrupt",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::InvalidSource => "invalid_source",
            Self::SourceChanged => "source_changed",
            Self::SourceDatabase => "source_database",
            Self::Io => "io",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_status_and_failure_strings_are_stable() {
        assert_eq!(ProviderSourceStatus::Available.as_str(), "available");
        assert_eq!(ProviderSourceStatus::Unsupported.as_str(), "unsupported");
        assert_eq!(
            ProviderSourceStatusReason::BlockedAuthOrEncryption.as_str(),
            "blocked_auth_or_encryption"
        );
        assert_eq!(
            ProviderSourceFailureKind::SchemaIncompatible.to_string(),
            "schema_incompatible"
        );
        assert_eq!(
            ProviderSourceFailureKind::SourceDatabase.to_string(),
            "source_database"
        );
    }

    #[test]
    fn import_support_predicates_are_stable() {
        assert!(ProviderImportSupport::Native.is_importable());
        assert!(ProviderImportSupport::Native.is_auto_importable());
        assert!(ProviderImportSupport::Explicit.is_importable());
        assert!(!ProviderImportSupport::Explicit.is_auto_importable());
        assert!(!ProviderImportSupport::Unsupported.is_importable());
    }
}
