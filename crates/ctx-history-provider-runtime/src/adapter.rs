pub use ctx_history_capture_model::{default_machine_id, ProviderAdapterContext};
use uuid::Uuid;

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::DateTime;

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
