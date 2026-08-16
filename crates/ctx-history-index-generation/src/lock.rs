use std::time::{Duration, Instant};

use tantivy::directory::{error::LockError, Directory, DirectoryLock, Lock};

use crate::{DurableMmapDirectory, Result};

const WRITER_HANDOFF_RETRY_WINDOW: Duration = Duration::from_millis(500);
const WRITER_HANDOFF_RETRY_INTERVAL: Duration = Duration::from_millis(5);

pub fn acquire_generation_writer_lock_with_retry(
    directory: &DurableMmapDirectory,
    lock: &Lock,
) -> Result<DirectoryLock> {
    let deadline = Instant::now() + WRITER_HANDOFF_RETRY_WINDOW;
    loop {
        match directory.acquire_lock(lock) {
            Ok(lock) => return Ok(lock),
            Err(error @ LockError::LockBusy) if Instant::now() >= deadline => {
                return Err(tantivy::TantivyError::LockFailure(
                    error,
                    Some(
                        "Failed to acquire index lock. If you are using a regular directory, this \
                         means there is already an `IndexWriter` working on this `Directory`, in \
                         this process or in a different process."
                            .to_owned(),
                    ),
                )
                .into());
            }
            Err(LockError::LockBusy) => {
                std::thread::sleep(WRITER_HANDOFF_RETRY_INTERVAL);
            }
            Err(error) => {
                return Err(tantivy::TantivyError::LockFailure(
                    error,
                    Some("failed to acquire the generation writer lock".to_owned()),
                )
                .into());
            }
        }
    }
}
