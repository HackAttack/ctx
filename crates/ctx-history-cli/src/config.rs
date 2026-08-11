use std::path::Path;

/// The single immutable configuration snapshot supplied for one history
/// invocation. Host-specific configuration representations never cross this
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCliConfig {
    pub daemon_enabled: bool,
    pub semantic_search_enabled: bool,
    pub local_usage_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("history CLI configuration operation failed: {message}")]
pub struct ConfigPortError {
    pub message: String,
}

impl ConfigPortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Configuration mutations that history setup is allowed to request.
///
/// The final binary owns parsing, durable writes, and the one post-write
/// snapshot conversion. This port intentionally does not expose the host's
/// full configuration object.
pub trait HistoryCliConfigPort {
    fn ensure_default_config(&mut self, data_root: &Path) -> Result<(), ConfigPortError>;

    fn set_semantic_search_enabled(
        &mut self,
        data_root: &Path,
        enabled: bool,
    ) -> Result<HistoryCliConfig, ConfigPortError>;
}
