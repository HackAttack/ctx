use std::{fs, io};

pub fn tempdir() -> io::Result<tempfile::TempDir> {
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    tempfile::Builder::new()
        .prefix("ctx-history-source-sqlite-")
        .tempdir_in(temp_root)
}
