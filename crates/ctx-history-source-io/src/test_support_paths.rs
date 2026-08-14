use std::{fs, io};

#[cfg(unix)]
use std::{ffi::CString, os::unix::ffi::OsStrExt as _, path::Path};

pub fn tempdir() -> io::Result<tempfile::TempDir> {
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    tempfile::Builder::new()
        .prefix("ctx-history-source-io-")
        .tempdir_in(temp_root)
}

#[cfg(unix)]
pub fn make_fifo(path: &Path) -> io::Result<()> {
    let raw = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "FIFO path contains NUL"))?;
    let result = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
