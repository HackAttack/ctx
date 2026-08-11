/// The single immutable configuration snapshot supplied for one history
/// invocation. Host-specific configuration representations never cross this
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCliConfig {
    pub daemon_enabled: bool,
    pub semantic_search_enabled: bool,
    pub local_usage_enabled: bool,
}

/// The narrow configuration authority needed by history setup adapters.
///
/// Hosts retain concrete configuration representation, persistence, mutation,
/// environment resolution, and error types. Command adapters use this port
/// statically so those final-host concerns do not cross into this crate.
pub trait HistoryConfigPort {
    type Error;

    fn snapshot(&self) -> HistoryCliConfig;

    fn write_default_config(&mut self) -> Result<(), Self::Error>;

    fn set_semantic_search_enabled(&mut self, enabled: bool) -> Result<(), Self::Error>;
}

/// Read-only configuration projection for history command application work.
/// Mutable persistence remains available only through `HistoryConfigPort` at
/// the final host boundary.
pub trait HistoryConfigSnapshotPort {
    fn snapshot(&self) -> HistoryCliConfig;
}

impl<T: HistoryConfigPort + ?Sized> HistoryConfigSnapshotPort for T {
    fn snapshot(&self) -> HistoryCliConfig {
        HistoryConfigPort::snapshot(self)
    }
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

#[cfg(test)]
mod tests {
    use super::{AppConfig, HistoryCliConfig, HistoryConfigPort};

    #[derive(Debug)]
    struct TestConfigPort {
        snapshot: HistoryCliConfig,
        wrote_default: bool,
        semantic_enabled: Option<bool>,
    }

    impl HistoryConfigPort for TestConfigPort {
        type Error = ();

        fn snapshot(&self) -> HistoryCliConfig {
            self.snapshot
        }

        fn write_default_config(&mut self) -> Result<(), Self::Error> {
            self.wrote_default = true;
            Ok(())
        }

        fn set_semantic_search_enabled(&mut self, enabled: bool) -> Result<(), Self::Error> {
            self.semantic_enabled = Some(enabled);
            self.snapshot.semantic_search_enabled = enabled;
            Ok(())
        }
    }

    fn configure_setup<P: HistoryConfigPort>(config: &mut P) -> Result<HistoryCliConfig, P::Error> {
        config.set_semantic_search_enabled(true)?;
        config.write_default_config()?;
        Ok(config.snapshot())
    }

    #[test]
    fn typed_port_keeps_setup_mutation_and_snapshot_host_defined() {
        let mut port = TestConfigPort {
            snapshot: HistoryCliConfig {
                daemon_enabled: true,
                semantic_search_enabled: false,
                local_usage_enabled: true,
            },
            wrote_default: false,
            semantic_enabled: None,
        };

        let snapshot = configure_setup(&mut port).unwrap();

        assert!(port.wrote_default);
        assert_eq!(port.semantic_enabled, Some(true));
        assert!(snapshot.semantic_search_enabled);
    }

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
