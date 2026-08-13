use ctx_history_capture::SourceBackedCurrentSourceProgress;
use ctx_history_source_io::{SqliteSourceProgress, SqliteSourceProgressStage};

#[test]
fn sqlite_progress_into_capture_reexport_compiles() {
    let source = SqliteSourceProgress::new(SqliteSourceProgressStage::SourceFamilyCopy);
    let _: SourceBackedCurrentSourceProgress = source.into();
}
