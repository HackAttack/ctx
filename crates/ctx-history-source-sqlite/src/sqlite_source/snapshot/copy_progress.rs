use super::*;
use fs2::FileExt as _;

#[cfg(unix)]
use std::os::unix::fs::FileExt as _;
#[cfg(windows)]
use std::os::windows::fs::FileExt as _;

const SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn copy_sqlite_member_with_progress<E>(
    member: &SqliteFamilyMember,
    destination: &Path,
    expected_length: u64,
    completed_bytes: &mut u64,
    last_reported_bytes: &mut u64,
    total_bytes: u64,
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<(), SqliteSourceProgressError<E>> {
    #[cfg(not(any(unix, windows)))]
    let mut source_file = {
        let mut source_file =
            member
                .file()
                .try_clone()
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "retaining a provider SQLite component for snapshot copy",
                    path: member.path.clone(),
                    source,
                })?;
        source_file
            .seek(SeekFrom::Start(0))
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "seeking a provider SQLite component for snapshot copy",
                path: member.path.clone(),
                source,
            })?;
        source_file
    };
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    if expected_length != 0 {
        destination_file
            .allocate(expected_length)
            .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "reserving space for a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
    }
    let mut remaining = expected_length;
    let mut buffer = [0_u8; SQLITE_COPY_BUFFER_BYTES];
    while remaining > 0 {
        #[cfg(any(unix, windows))]
        let source_offset = expected_length - remaining;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        #[cfg(any(unix, windows))]
        let read = read_source_at(member.file(), &mut buffer[..requested], source_offset);
        #[cfg(not(any(unix, windows)))]
        let read = source_file.read(&mut buffer[..requested]);
        let read = read.map_err(|source| SqliteSourceAccessError::Io {
            operation: "reading a provider SQLite snapshot component",
            path: member.path.clone(),
            source,
        })?;
        if read == 0 {
            return Err(SqliteSourceAccessError::SourceChanged.into());
        }
        destination_file
            .write_all(&buffer[..read])
            .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "writing a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
        remaining -= read as u64;
        *completed_bytes = completed_bytes.checked_add(read as u64).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the SQLite source-family copy progress count overflowed".to_owned(),
            }
        })?;
        if *completed_bytes == total_bytes
            || completed_bytes.saturating_sub(*last_reported_bytes)
                >= SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES
        {
            report_source_family_copy_progress(report_progress, *completed_bytes, total_bytes)?;
            *last_reported_bytes = *completed_bytes;
        }
    }
    let mut extra = [0_u8; 1];
    #[cfg(any(unix, windows))]
    let extra_read = read_source_at(member.file(), &mut extra, expected_length);
    #[cfg(not(any(unix, windows)))]
    let extra_read = source_file.read(&mut extra);
    if extra_read.map_err(|source| SqliteSourceAccessError::Io {
        operation: "certifying a provider SQLite snapshot component length",
        path: member.path.clone(),
        source,
    })? != 0
    {
        return Err(SqliteSourceAccessError::SourceChanged.into());
    }
    destination_file
        .flush()
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "flushing a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(unix)]
fn read_source_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_source_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    file.seek_read(buffer, offset)
}

pub(super) fn report_source_family_copy_progress<E>(
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    completed_bytes: u64,
    total_bytes: u64,
) -> Result<(), SqliteSourceProgressError<E>> {
    let mut progress = SqliteSourceProgress::new(SqliteSourceProgressStage::SourceFamilyCopy);
    progress.snapshot_bytes_completed = Some(completed_bytes);
    progress.snapshot_bytes_total = Some(total_bytes);
    report_progress(progress).map_err(SqliteSourceProgressError::Progress)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn positional_source_read_does_not_change_the_retained_handle_offset() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite");
        std::fs::write(&source_path, b"0123456789").unwrap();
        let mut source = File::open(&source_path).unwrap();
        source.seek(SeekFrom::Start(7)).unwrap();
        let mut observed = [0_u8; 4];

        assert_eq!(read_source_at(&source, &mut observed, 2).unwrap(), 4);

        assert_eq!(&observed, b"2345");
        assert_eq!(source.stream_position().unwrap(), 7);
    }
}
