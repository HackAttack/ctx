use std::{env, path::PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::utc_now;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub source_root: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
}

impl ProviderAdapterContext {
    pub fn source_root_display(&self) -> Option<String> {
        self.source_root
            .as_ref()
            .or(self.source_path.as_ref())
            .map(|path| path.display().to_string())
    }
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            source_root: None,
            imported_at: utc_now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderImportOptions {
    pub history_record_id: Option<Uuid>,
    pub capture_work_limit: CaptureWorkLimit,
    pub inventory_observation_token: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureWorkLimit {
    #[default]
    Drain,
    OneSafeGroup,
}

impl Default for ProviderImportOptions {
    fn default() -> Self {
        Self {
            history_record_id: None,
            capture_work_limit: CaptureWorkLimit::Drain,
            inventory_observation_token: None,
        }
    }
}

pub fn default_machine_id() -> String {
    env::var("CTX_MACHINE_ID")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_root_precedes_source_path_in_display_context() {
        let context = ProviderAdapterContext {
            machine_id: "machine".to_owned(),
            source_path: Some(PathBuf::from("fallback")),
            source_root: Some(PathBuf::from("authority")),
            imported_at: DateTime::UNIX_EPOCH,
        };
        assert_eq!(context.source_root_display().as_deref(), Some("authority"));
    }
}
