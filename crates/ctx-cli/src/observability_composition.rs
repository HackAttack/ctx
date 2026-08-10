use std::path::{Path, PathBuf};

use crate::{
    analytics::{pro_operation_event, Outcome, PublicEventV1},
    config::{AppConfig, LocalUsageConfigResolver, LocalUsageConfigState},
    local_usage::{UsageControlRevision, UsageControlSnapshot},
};

pub(crate) fn tool_product_event(
    observation: crate::tool_backend::ToolProductObservation,
) -> PublicEventV1 {
    pro_operation_event(
        observation.operation,
        if observation.success {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        observation.duration,
    )
}

const LOCAL_USAGE_DATABASE_FILE: &str = "usage.sqlite";

/// Exact-path capability minted only by outer observability composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalUsageStorageAuthority {
    database_path: PathBuf,
}

impl LocalUsageStorageAuthority {
    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }
}

pub(crate) fn local_usage_storage_authority(data_root: &Path) -> LocalUsageStorageAuthority {
    LocalUsageStorageAuthority {
        database_path: data_root.join(LOCAL_USAGE_DATABASE_FILE),
    }
}

pub(crate) const fn usage_control_snapshot(enabled: bool) -> UsageControlSnapshot {
    UsageControlSnapshot::unversioned(enabled)
}

fn usage_control_revision(config_path: &Path) -> Option<UsageControlRevision> {
    observe_usage_control_metadata_read();
    match config_path.metadata() {
        Ok(metadata) => UsageControlRevision::from_file_metadata(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Some(UsageControlRevision::missing())
        }
        Err(_) => None,
    }
}

#[cfg(test)]
thread_local! {
    static USAGE_CONTROL_METADATA_READ_COUNT: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
}

fn observe_usage_control_metadata_read() {
    #[cfg(test)]
    USAGE_CONTROL_METADATA_READ_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
}

#[cfg(test)]
pub(crate) fn count_usage_control_metadata_reads<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    USAGE_CONTROL_METADATA_READ_COUNT.with(|count| {
        let previous = count.replace(Some(0));
        assert!(
            previous.is_none(),
            "usage-control metadata counters must not be nested"
        );
        let result = operation();
        let observed = count.replace(previous).unwrap_or(0);
        (result, observed)
    })
}

fn stable_usage_control_snapshot(
    enabled: bool,
    revision_before: Option<UsageControlRevision>,
    revision_after: Option<UsageControlRevision>,
) -> UsageControlSnapshot {
    let revision = match (revision_before, revision_after) {
        (Some(before), Some(after)) if before == after => Some(after),
        _ => None,
    };
    UsageControlSnapshot::new(enabled, revision)
}

/// Path-aware local-usage policy resolver owned by process composition.
/// Observability receives only path-free snapshots from this authority.
pub(crate) struct LocalUsageControlAuthority {
    data_root: PathBuf,
    resolver: LocalUsageConfigResolver,
    previous: Option<bool>,
}

impl LocalUsageControlAuthority {
    pub(crate) fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            resolver: LocalUsageConfigResolver::default(),
            previous: None,
        }
    }

    pub(crate) fn snapshot(&mut self) -> UsageControlSnapshot {
        let config_path = AppConfig::config_path(&self.data_root);
        let before = usage_control_revision(&config_path);
        let resolution = self.resolver.resolve(&self.data_root);
        let available = matches!(resolution.config_state, LocalUsageConfigState::Resolved(_));
        let enabled = resolution.effective_after(self.previous);
        let after = usage_control_revision(&config_path);
        self.previous = Some(enabled);
        let snapshot = stable_usage_control_snapshot(enabled, before, after);
        if available {
            snapshot
        } else {
            UsageControlSnapshot::unavailable(enabled, snapshot.revision().cloned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_authority_grants_only_the_exact_legacy_database_path() {
        let root = Path::new("/tmp/ctx-observability-authority-test");
        assert_eq!(
            local_usage_storage_authority(root).database_path(),
            root.join("usage.sqlite")
        );
    }
}
