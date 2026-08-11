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
