use std::{
    path::Path,
    time::{Duration, Instant},
};

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

/// Waits for the current ownership handoff to finish so a reader can
/// atomically select and lease one retained generation.
///
/// Unlike writer admission, this has no handoff timeout: returning `LockBusy`
/// would make a valid query fail merely because publication was in progress.
/// The reader holds the returned fence only for bounded certification work and
/// therefore cannot couple publication to manifest materialization.
pub struct GenerationOwnershipFence {
    _lock: DirectoryLock,
}

/// Acquires the short gate shared by reader pinning, pointer handoff, and
/// reclamation. Candidate construction does not hold this gate.
pub fn acquire_generation_ownership_fence(
    root: impl AsRef<Path>,
) -> Result<GenerationOwnershipFence> {
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    acquire_generation_ownership_fence_in(&directory)
}

pub(crate) fn acquire_generation_ownership_fence_in(
    directory: &DurableMmapDirectory,
) -> Result<GenerationOwnershipFence> {
    let lock = Lock {
        filepath: crate::GENERATION_OWNERSHIP_LOCK_FILE.into(),
        is_blocking: true,
    };
    directory
        .acquire_lock(&lock)
        .map(|lock| GenerationOwnershipFence { _lock: lock })
        .map_err(|error| {
            tantivy::TantivyError::LockFailure(
                error,
                Some("failed to acquire the generation ownership fence".to_owned()),
            )
            .into()
        })
}
