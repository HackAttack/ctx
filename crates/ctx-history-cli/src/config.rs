use std::path::Path;

use anyhow::Result;

/// The single immutable configuration snapshot supplied for one history
/// invocation. Host-specific configuration representations never cross this
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCliConfig {
    pub daemon_enabled: bool,
    pub semantic_search_enabled: bool,
    pub local_usage_enabled: bool,
}

/// A command-local projection of the daemon-owned configuration snapshot.
/// It deliberately carries no mutable host configuration authority.
#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) daemon: DaemonConfig,
    pub(crate) local_usage: LocalUsageConfig,
    semantic_enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DaemonConfig {
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalUsageConfig {
    pub(crate) enabled: bool,
}

impl AppConfig {
    pub(crate) const fn from_snapshot(config: HistoryCliConfig) -> Self {
        Self {
            daemon: DaemonConfig {
                enabled: config.daemon_enabled,
            },
            // Usage persistence is final-binary-owned; this only permits the
            // existing bounded draft computation before that adapter decides
            // whether to retain it.
            local_usage: LocalUsageConfig {
                enabled: config.local_usage_enabled,
            },
            semantic_enabled: config.semantic_search_enabled,
        }
    }

    pub(crate) const fn semantic_search_enabled(&self) -> bool {
        self.semantic_enabled
    }
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
    /// Returns the sole immutable snapshot for an invocation. Command bodies
    /// must reuse this value rather than re-reading host configuration.
    fn snapshot(&self, data_root: &Path) -> Result<HistoryCliConfig, ConfigPortError>;

    fn ensure_default_config(&mut self, data_root: &Path) -> Result<(), ConfigPortError>;

    fn set_semantic_search_enabled(
        &mut self,
        data_root: &Path,
        enabled: bool,
    ) -> Result<HistoryCliConfig, ConfigPortError>;
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, HistoryCliConfig};

    #[test]
    fn snapshot_preserves_disabled_local_usage() {
        let config = AppConfig::from_snapshot(HistoryCliConfig {
            daemon_enabled: false,
            semantic_search_enabled: true,
            local_usage_enabled: false,
        });

        assert!(!config.daemon.enabled);
        assert!(config.semantic_search_enabled());
        assert!(!config.local_usage.enabled);
    }

    #[test]
    fn snapshot_preserves_enabled_local_usage() {
        let config = AppConfig::from_snapshot(HistoryCliConfig {
            daemon_enabled: true,
            semantic_search_enabled: false,
            local_usage_enabled: true,
        });

        assert!(config.daemon.enabled);
        assert!(!config.semantic_search_enabled());
        assert!(config.local_usage.enabled);
    }
}
