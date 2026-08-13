use ctx_history_capture_model::{
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSourceProgressStage {
    SourceFamilyCopy,
}

impl SqliteSourceProgressStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteSourceProgress {
    pub stage: SqliteSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
}

impl SqliteSourceProgress {
    pub const fn new(stage: SqliteSourceProgressStage) -> Self {
        Self {
            stage,
            snapshot_pages_completed: None,
            snapshot_pages_total: None,
            snapshot_bytes_completed: None,
            snapshot_bytes_total: None,
        }
    }
}

impl From<SqliteSourceProgress> for SourceBackedCurrentSourceProgress {
    fn from(progress: SqliteSourceProgress) -> Self {
        let stage = match progress.stage {
            SqliteSourceProgressStage::SourceFamilyCopy => {
                SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            }
        };
        Self {
            stage,
            snapshot_pages_completed: progress.snapshot_pages_completed,
            snapshot_pages_total: progress.snapshot_pages_total,
            snapshot_bytes_completed: progress.snapshot_bytes_completed,
            snapshot_bytes_total: progress.snapshot_bytes_total,
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_progress_conversion_preserves_every_field() {
        let source = SqliteSourceProgress {
            stage: SqliteSourceProgressStage::SourceFamilyCopy,
            snapshot_pages_completed: Some(3),
            snapshot_pages_total: Some(5),
            snapshot_bytes_completed: Some(12_288),
            snapshot_bytes_total: Some(20_480),
        };
        assert_eq!(
            SourceBackedCurrentSourceProgress::from(source),
            SourceBackedCurrentSourceProgress {
                stage: SourceBackedCurrentSourceProgressStage::SourceFamilyCopy,
                snapshot_pages_completed: Some(3),
                snapshot_pages_total: Some(5),
                snapshot_bytes_completed: Some(12_288),
                snapshot_bytes_total: Some(20_480),
                logical_rows_scanned: None,
                logical_certified_bytes: None,
            }
        );
    }
}
