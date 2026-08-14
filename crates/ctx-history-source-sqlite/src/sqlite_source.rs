//! Stock SQLite snapshots for root-authorized provider databases.
//!
//! The ordinary provider-source layer approves and retains the database parent
//! directory. This module keeps that [`ProviderSourceDirectory`] capability,
//! opens every DB/WAL/SHM/journal leaf relative to it, rejects symlink,
//! reparse-point, cross-filesystem, and non-regular members, and never asks
//! SQLite to create or update files in the provider directory.
//!
//! The exact-policy path opens a sidecar-free database through SQLite's
//! immutable URI mode when the platform supports it. Every other route copies
//! one exact DB/WAL family, with bounded I/O, to one private directory below the
//! ctx data root. Family-member replacement or appearance remains fail-closed.
//! Rollback journals remain typed unavailable because recovery could require
//! database writes. SHM is bounded volatile lock coordination; provider
//! DB/WAL/SHM bytes and directory entries are never mutated.

use std::{
    ffi::{c_char, c_void, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    ptr,
    sync::{Arc, Mutex, MutexGuard},
};

use ctx_history_core::platform_security::create_private_directory_all;
use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
#[cfg(target_os = "linux")]
use url::Url;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

use ctx_history_source_io::{
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
    ProviderSourceRoot, SourceIoError,
};

use crate::{SqliteSourceProgress, SqliteSourceProgressStage};

const EVIDENCE_DOMAIN: &[u8] = b"ctx-stock-sqlite-snapshot-v2\0";
// Admit an approximately 1 GiB provider database together with an active WAL
// of comparable size while retaining one finite cumulative copy bound.
const SQLITE_SNAPSHOT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES: u64 = 16 * 1024 * 1024;
const SQLITE_COPY_BUFFER_BYTES: usize = 64 * 1024;
const SQLITE_WAL_TOKEN_BYTES: usize = 64;
const SQLITE_SHM_MAX_BYTES: u64 = 8 * 1024 * 1024;

mod diagnostics;
pub use diagnostics::{
    resource_exhaustion_io_error, rusqlite_busy_or_locked, rusqlite_resource_failure,
    sqlite_retry_decision, SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase,
    SqliteRetryDecision, SqliteSourceAccessError, SqliteSourceComponent,
    SqliteSourceErrorComposition, SqliteSourceProgressError,
};

pub type SqliteSourceAccessResult<T> = Result<T, SqliteSourceAccessError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSourceSnapshotStrategy {
    #[cfg(target_os = "linux")]
    ImmutableMain,
    #[cfg(target_os = "linux")]
    PinnedReadOnlyWal,
    CopiedFamily,
}

/// Selects how one authorized provider SQLite leaf is stabilized.
///
/// Both policies acquire the same physical files. The stable-copy policy keeps
/// its private copy readable while the source's retained database identity is
/// still present; interpretation and publication policy remain with capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteSourceSnapshotPolicy {
    ExactRevision,
    PinnedReadOnlyWal,
    StablePrivateCopy,
}

/// Content-free work and concurrency counters for one retained SQLite
/// directory authority and all snapshots opened through its clones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SqliteSourceSnapshotCounters {
    immutable_snapshot_opens: u64,
    pinned_read_only_wal_snapshot_opens: u64,
    copied_snapshot_opens: u64,
    source_bytes_copied: u64,
    terminal_fences: u64,
    terminal_revalidations: u64,
    explicit_aborts: u64,
    unfinished_drops: u64,
    scratch_admissions: u64,
    max_route_scratch_bytes: u64,
    active_snapshots: u64,
    active_snapshot_bytes: u64,
    max_active_snapshots: u64,
    max_active_snapshot_bytes: u64,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct SqliteSourceSnapshotCounterObserver {
    context: Arc<SqliteSourceSnapshotContext>,
}

#[cfg(any(test, feature = "test-support"))]
impl SqliteSourceSnapshotCounterObserver {
    pub fn snapshot(&self) -> SqliteSourceSnapshotCounters {
        self.context.snapshot()
    }
}

impl SqliteSourceSnapshotCounters {
    pub const fn immutable_snapshot_opens(self) -> u64 {
        self.immutable_snapshot_opens
    }

    pub const fn copied_snapshot_opens(self) -> u64 {
        self.copied_snapshot_opens
    }

    pub const fn pinned_read_only_wal_snapshot_opens(self) -> u64 {
        self.pinned_read_only_wal_snapshot_opens
    }

    pub const fn source_bytes_copied(self) -> u64 {
        self.source_bytes_copied
    }

    pub const fn terminal_fences(self) -> u64 {
        self.terminal_fences
    }

    pub const fn terminal_revalidations(self) -> u64 {
        self.terminal_revalidations
    }

    pub const fn explicit_aborts(self) -> u64 {
        self.explicit_aborts
    }

    pub const fn unfinished_drops(self) -> u64 {
        self.unfinished_drops
    }

    pub const fn scratch_admissions(self) -> u64 {
        self.scratch_admissions
    }

    pub const fn max_route_scratch_bytes(self) -> u64 {
        self.max_route_scratch_bytes
    }

    pub const fn active_snapshots(self) -> u64 {
        self.active_snapshots
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn active_snapshot_bytes(self) -> u64 {
        self.active_snapshot_bytes
    }

    pub const fn max_active_snapshots(self) -> u64 {
        self.max_active_snapshots
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn max_active_snapshot_bytes(self) -> u64 {
        self.max_active_snapshot_bytes
    }
}

#[derive(Debug)]
struct SqliteSourceSnapshotContext {
    data_root: PathBuf,
    counters: Mutex<SqliteSourceSnapshotCounters>,
}

impl SqliteSourceSnapshotContext {
    fn snapshot(&self) -> SqliteSourceSnapshotCounters {
        *self.lock()
    }

    fn record_source_bytes_copied(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.source_bytes_copied =
            checked_counter_add(counters.source_bytes_copied, bytes, "source bytes copied")?;
        Ok(())
    }

    fn record_open(
        self: &Arc<Self>,
        strategy: SqliteSourceSnapshotStrategy,
        active_bytes: u64,
    ) -> SqliteSourceAccessResult<SqliteSourceSnapshotActivity> {
        let mut counters = self.lock();
        let mut next = *counters;
        match strategy {
            #[cfg(target_os = "linux")]
            SqliteSourceSnapshotStrategy::ImmutableMain => {
                next.immutable_snapshot_opens = checked_counter_add(
                    next.immutable_snapshot_opens,
                    1,
                    "immutable snapshot opens",
                )?;
            }
            SqliteSourceSnapshotStrategy::CopiedFamily => {
                next.copied_snapshot_opens =
                    checked_counter_add(next.copied_snapshot_opens, 1, "copied snapshot opens")?;
            }
            #[cfg(target_os = "linux")]
            SqliteSourceSnapshotStrategy::PinnedReadOnlyWal => {
                next.pinned_read_only_wal_snapshot_opens = checked_counter_add(
                    next.pinned_read_only_wal_snapshot_opens,
                    1,
                    "direct read-only snapshot opens",
                )?;
            }
        }
        next.active_snapshots = checked_counter_add(next.active_snapshots, 1, "active snapshots")?;
        next.active_snapshot_bytes = checked_counter_add(
            next.active_snapshot_bytes,
            active_bytes,
            "active snapshot bytes",
        )?;
        next.max_active_snapshots = next.max_active_snapshots.max(next.active_snapshots);
        next.max_active_snapshot_bytes = next
            .max_active_snapshot_bytes
            .max(next.active_snapshot_bytes);
        *counters = next;
        drop(counters);
        Ok(SqliteSourceSnapshotActivity {
            context: Arc::clone(self),
            active_bytes,
        })
    }

    fn record_terminal_fence(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_fences =
            checked_counter_add(counters.terminal_fences, 1, "terminal fences")?;
        Ok(())
    }

    fn record_terminal_revalidation(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_revalidations =
            checked_counter_add(counters.terminal_revalidations, 1, "terminal revalidations")?;
        Ok(())
    }

    fn record_explicit_abort(&self) {
        let mut counters = self.lock();
        counters.explicit_aborts = counters.explicit_aborts.saturating_add(1);
    }

    fn record_unfinished_drop(&self) {
        let mut counters = self.lock();
        counters.unfinished_drops = counters.unfinished_drops.saturating_add(1);
    }

    fn record_scratch_admission(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.scratch_admissions =
            checked_counter_add(counters.scratch_admissions, 1, "scratch admissions")?;
        Ok(())
    }

    fn record_route_scratch_peak(&self, bytes: u64) {
        let mut counters = self.lock();
        counters.max_route_scratch_bytes = counters.max_route_scratch_bytes.max(bytes);
    }

    fn lock(&self) -> MutexGuard<'_, SqliteSourceSnapshotCounters> {
        match self.counters.lock() {
            Ok(counters) => counters,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
struct SqliteSourceSnapshotActivity {
    context: Arc<SqliteSourceSnapshotContext>,
    active_bytes: u64,
}

impl Drop for SqliteSourceSnapshotActivity {
    fn drop(&mut self) {
        let mut counters = self.context.lock();
        counters.active_snapshots = counters.active_snapshots.saturating_sub(1);
        counters.active_snapshot_bytes = counters
            .active_snapshot_bytes
            .saturating_sub(self.active_bytes);
    }
}

#[derive(Debug, Default)]
struct SqliteRouteScratchState {
    retained_bytes: u64,
    reserved_transient_bytes: u64,
    exact_peak_bytes: u64,
}

/// One physical scratch ledger for every retained and transient file created
/// by a single SQLite route.
#[derive(Debug)]
struct SqliteRouteScratch {
    context: Arc<SqliteSourceSnapshotContext>,
    maximum_bytes: u64,
    state: Mutex<SqliteRouteScratchState>,
}

impl SqliteRouteScratch {
    fn new(context: &Arc<SqliteSourceSnapshotContext>, maximum_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            context: Arc::clone(context),
            maximum_bytes,
            state: Mutex::new(SqliteRouteScratchState::default()),
        })
    }

    fn admit_capacity(&self, capacity_bytes: u64) -> SqliteSourceAccessResult<()> {
        if capacity_bytes > self.maximum_bytes {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: capacity_bytes,
                maximum: self.maximum_bytes,
            });
        }
        let required = capacity_bytes
            .checked_add(SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: u64::MAX,
                maximum: self.maximum_bytes,
            })?;
        let available = scratch_available_space(&self.context.data_root)?;
        if available < required {
            return Err(SqliteSourceAccessError::InsufficientScratchSpace {
                path: self.context.data_root.clone(),
                required,
                available,
            });
        }
        self.context.record_scratch_admission()
    }

    fn set_retained_bytes(&self, retained_bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut state = self.lock();
        let aggregate = retained_bytes
            .checked_add(state.reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: u64::MAX,
                maximum: self.maximum_bytes,
            })?;
        if aggregate > self.maximum_bytes {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: aggregate,
                maximum: self.maximum_bytes,
            });
        }
        state.retained_bytes = retained_bytes;
        state.exact_peak_bytes = state.exact_peak_bytes.max(retained_bytes);
        self.context
            .record_route_scratch_peak(state.exact_peak_bytes);
        Ok(())
    }

    fn reserve_transient(
        self: &Arc<Self>,
        requested_bytes: u64,
    ) -> SqliteSourceAccessResult<SqliteRouteScratchReservation> {
        let state = self.lock();
        let used = state
            .retained_bytes
            .checked_add(state.reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: u64::MAX,
                maximum: self.maximum_bytes,
            })?;
        let available_capacity = self.maximum_bytes.saturating_sub(used);
        let reserved_bytes = requested_bytes.min(available_capacity);
        if reserved_bytes == 0 {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: requested_bytes,
                maximum: available_capacity,
            });
        }
        drop(state);
        self.admit_capacity(reserved_bytes)?;
        let mut state = self.lock();
        let reserved_transient_bytes =
            state
                .reserved_transient_bytes
                .checked_add(reserved_bytes)
                .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                    path: self.context.data_root.clone(),
                    length: u64::MAX,
                    maximum: self.maximum_bytes,
                })?;
        let aggregate = state
            .retained_bytes
            .checked_add(reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: u64::MAX,
                maximum: self.maximum_bytes,
            })?;
        if aggregate > self.maximum_bytes {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: aggregate,
                maximum: self.maximum_bytes,
            });
        }
        state.reserved_transient_bytes = reserved_transient_bytes;
        Ok(SqliteRouteScratchReservation {
            account: Arc::clone(self),
            reserved_bytes,
        })
    }

    fn record_transient_bytes(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut state = self.lock();
        if bytes > state.reserved_transient_bytes {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: bytes,
                maximum: state.reserved_transient_bytes,
            });
        }
        let aggregate = state.retained_bytes.checked_add(bytes).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: u64::MAX,
                maximum: self.maximum_bytes,
            }
        })?;
        state.exact_peak_bytes = state.exact_peak_bytes.max(aggregate);
        self.context
            .record_route_scratch_peak(state.exact_peak_bytes);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, SqliteRouteScratchState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
struct SqliteRouteScratchReservation {
    account: Arc<SqliteRouteScratch>,
    reserved_bytes: u64,
}

impl SqliteRouteScratchReservation {
    fn maximum_bytes(&self) -> u64 {
        self.reserved_bytes
    }

    fn record_exact_bytes(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        self.account.record_transient_bytes(bytes)
    }
}

impl Drop for SqliteRouteScratchReservation {
    fn drop(&mut self) {
        let mut state = self.account.lock();
        state.reserved_transient_bytes = state
            .reserved_transient_bytes
            .saturating_sub(self.reserved_bytes);
    }
}

fn scratch_available_space(path: &Path) -> SqliteSourceAccessResult<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(available) = take_scratch_available_space_override() {
        return Ok(available);
    }
    let mut measurement_path = path;
    loop {
        match std::fs::metadata(measurement_path) {
            Ok(_) => break,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                measurement_path = measurement_path.parent().ok_or_else(|| {
                    SqliteSourceAccessError::ScratchIoUnavailable {
                        operation: "locating an existing SQLite scratch filesystem ancestor",
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            }
            Err(source) => {
                return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                    operation: "locating an existing SQLite scratch filesystem ancestor",
                    path: measurement_path.to_path_buf(),
                    source,
                });
            }
        }
    }
    fs2::available_space(measurement_path).map_err(|source| {
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "measuring available provider SQLite scratch space",
            path: measurement_path.to_path_buf(),
            source,
        }
    })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCRATCH_AVAILABLE_SPACE_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
fn take_scratch_available_space_override() -> Option<u64> {
    SCRATCH_AVAILABLE_SPACE_OVERRIDE.with(|available| available.take())
}

#[cfg(any(test, feature = "test-support"))]
pub fn override_next_scratch_available_space_for_test(available: u64) {
    SCRATCH_AVAILABLE_SPACE_OVERRIDE.with(|slot| slot.set(Some(available)));
}

fn checked_counter_add(
    value: u64,
    increment: u64,
    counter: &'static str,
) -> SqliteSourceAccessResult<u64> {
    value
        .checked_add(increment)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("SQLite snapshot accounting overflowed {counter}"),
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSourceEvidence {
    identity: [u8; 32],
    length: u64,
    wal_length: Option<u64>,
    shared_memory_length: Option<u64>,
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
    revision: [u8; 32],
}

impl SqliteSourceEvidence {
    pub fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn wal_length(&self) -> Option<u64> {
        self.wal_length
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory_length
    }
}

/// Retained authority for one approved SQLite parent directory.
///
/// `path` is retained only to certify the parent route and describe errors.
/// SQLite family members are always opened relative to `directory`.
#[derive(Debug, Clone)]
pub struct SqliteSourceDirectoryAuthority {
    directory: Arc<ProviderSourceDirectory>,
    path: PathBuf,
    identity: NativeFileIdentity,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
}

impl SqliteSourceDirectoryAuthority {
    fn retain(
        data_root: &Path,
        authorized_parent: &File,
        approved_path: &Path,
    ) -> SqliteSourceAccessResult<Self> {
        validate_approved_parent_path(approved_path)?;
        let retained = NativeFileState::read(
            authorized_parent,
            approved_path,
            ExpectedObjectKind::Directory,
        )?;
        let root = ProviderSourceRoot::open(approved_path).map_err(|error| {
            map_provider_source_error(
                error,
                "opening the approved SQLite parent capability",
                approved_path,
            )
        })?;
        let directory = root.directory().map_err(|error| {
            map_provider_source_error(
                error,
                "retaining the approved SQLite parent capability",
                approved_path,
            )
        })?;
        let named = directory.try_clone_authority_handle().map_err(|source| {
            SqliteSourceAccessError::Io {
                operation: "retaining the approved SQLite parent capability handle",
                path: approved_path.to_path_buf(),
                source,
            }
        })?;
        let named_state =
            NativeFileState::read(&named, approved_path, ExpectedObjectKind::Directory)?;
        if retained.identity != named_state.identity {
            return Err(SqliteSourceAccessError::ConnectionIdentityMismatch);
        }
        Ok(Self {
            directory: Arc::new(directory),
            path: approved_path.to_path_buf(),
            identity: retained.identity,
            snapshot_context: Arc::new(SqliteSourceSnapshotContext {
                data_root: data_root.to_path_buf(),
                counters: Mutex::new(SqliteSourceSnapshotCounters::default()),
            }),
        })
    }

    pub fn snapshot_counters(&self) -> SqliteSourceSnapshotCounters {
        self.snapshot_context.snapshot()
    }

    /// Observes one bounded physical DB/WAL family revision without copying or
    /// opening a logical SQLite snapshot. The returned token is valid only for
    /// exact replay and must be observed again during terminal revalidation.
    pub fn observe_physical_revision(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<[u8; 32]> {
        let family = SqliteSourceFamily::open(self, database_name, || {})?;
        let evidence = family.capture_revision_evidence()?;
        family.revalidate_revision(&evidence)?;
        Ok(evidence.revision_token())
    }

    /// Acquires one private, exact copy of the currently authorized DB/WAL
    /// family. Same-object source writes after acquisition do not alter the
    /// copy, but replacing the retained database fails terminal revalidation.
    pub fn open_stable_snapshot(
        &self,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_policy(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::StablePrivateCopy,
            SqliteSourceSnapshotLimits::default(),
        )
    }

    pub fn open_stable_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_progress(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::StablePrivateCopy,
            SqliteSourceSnapshotLimits::default(),
            &mut report_progress,
        )
    }

    /// Opens one named provider DB/WAL view through SQLite's read-only SHM URI
    /// mode. The pinned transaction is coherent while WAL growth remains
    /// available to a successor refresh and provider bytes stay untouched.
    pub fn open_incremental_snapshot_with_progress<E>(
        &self,
        database_name: &OsStr,
        mut report_progress: impl FnMut(SqliteSourceProgress) -> Result<(), E>,
    ) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
        snapshot::open_root_handle_sqlite_source_snapshot_with_progress(
            self,
            database_name,
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal,
            SqliteSourceSnapshotLimits::default(),
            &mut report_progress,
        )
    }

    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained = self
            .directory
            .try_clone_authority_handle()
            .map_err(|source| {
                map_revalidation_io_error(
                    source,
                    "retaining the approved SQLite parent capability during revalidation",
                    &self.path,
                )
            })
            .and_then(|directory| {
                NativeFileState::read(&directory, &self.path, ExpectedObjectKind::Directory)
                    .map_err(map_revalidation_error)
            })?;
        if retained.identity != self.identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named_root = ProviderSourceRoot::open(&self.path).map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "reopening the approved SQLite parent capability during revalidation",
                &self.path,
            )
        })?;
        let named_directory = named_root.directory().map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "retaining the reopened SQLite parent capability during revalidation",
                &self.path,
            )
        })?;
        let named = named_directory
            .try_clone_authority_handle()
            .map_err(|source| {
                map_revalidation_io_error(
                    source,
                    "retaining the reopened SQLite parent capability handle during revalidation",
                    &self.path,
                )
            })?;
        let named_state = NativeFileState::read(&named, &self.path, ExpectedObjectKind::Directory)
            .map_err(map_revalidation_error)?;
        if named_state.identity == self.identity {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }
}

/// A sealed compact witness for the exact SQLite family that backed one
/// completed read snapshot.
///
/// The witness retains no provider handles. Commit-time validation reopens the
/// approved parent through the same no-follow capability path, certifies the
/// main database, any admitted WAL, and relevant SHM identity. This bounds live
/// descriptors by active workers rather than total discovered databases.
#[must_use = "revalidate the terminal fence before publishing snapshot observations"]
#[derive(Debug)]
struct SqliteSourceTerminalFenceInner {
    data_root: PathBuf,
    approved_parent_path: PathBuf,
    database_name: OsString,
    native_evidence: SqliteFamilyEvidence,
    evidence: SqliteSourceEvidence,
    policy: SqliteSourceSnapshotPolicy,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
}

#[derive(Clone, Debug)]
pub struct SqliteSourceTerminalFence {
    inner: Arc<SqliteSourceTerminalFenceInner>,
}

impl SqliteSourceTerminalFence {
    pub fn evidence(&self) -> &SqliteSourceEvidence {
        &self.inner.evidence
    }

    /// Revalidates the exact retained source family without opening SQLite or
    /// acquiring another source snapshot.
    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let root = ProviderSourceRoot::open(&self.inner.approved_parent_path).map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "reopening the approved SQLite parent for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let directory = root.directory().map_err(|error| {
            map_provider_source_revalidation_error(
                error,
                "retaining the reopened SQLite parent for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let authority_handle = directory.try_clone_authority_handle().map_err(|source| {
            map_revalidation_io_error(
                source,
                "retaining the reopened SQLite parent handle for terminal revalidation",
                &self.inner.approved_parent_path,
            )
        })?;
        let authority = SqliteSourceDirectoryAuthority::retain(
            &self.inner.data_root,
            &authority_handle,
            &self.inner.approved_parent_path,
        )
        .map_err(map_revalidation_error)?;
        match self.inner.policy {
            SqliteSourceSnapshotPolicy::ExactRevision => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                family.revalidate(&self.inner.native_evidence)?;
            }
            SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                family.revalidate_database_identity(&self.inner.native_evidence)?;
            }
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => {
                let family = SqliteSourceFamily::open(&authority, &self.inner.database_name, || {})
                    .map_err(map_revalidation_error)?;
                revalidate_live_database_schema(
                    &family,
                    &self.inner.native_evidence,
                    &self.inner.evidence.schema,
                )?;
            }
        }
        self.inner.snapshot_context.record_terminal_revalidation()
    }
}

#[derive(Debug, Default)]
struct SqliteSourceTerminalFenceSlot {
    fence: Mutex<Option<SqliteSourceTerminalFence>>,
}

impl SqliteSourceTerminalFenceSlot {
    fn install(&self, fence: SqliteSourceTerminalFence) -> SqliteSourceAccessResult<()> {
        let mut retained =
            self.fence
                .lock()
                .map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the retained SQLite terminal fence lock was poisoned".to_owned(),
                })?;
        if retained.is_some() {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the SQLite snapshot published more than one terminal fence".to_owned(),
            });
        }
        *retained = Some(fence);
        Ok(())
    }

    fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained =
            self.fence
                .lock()
                .map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "the retained SQLite terminal fence lock was poisoned".to_owned(),
                })?;
        retained
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?
            .revalidate()
    }
}

/// A stock read-only SQLite connection with a pinned read transaction.
#[must_use = "call seal() or finish() after provider queries and before publishing observations"]
#[derive(Debug)]
pub struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    family: Option<SqliteSourceFamily>,
    native_evidence: SqliteFamilyEvidence,
    sqlite_evidence: SqliteSnapshotEvidence,
    evidence: SqliteSourceEvidence,
    policy: SqliteSourceSnapshotPolicy,
    admitted_revision_is_replay_safe: bool,
    strategy: SqliteSourceSnapshotStrategy,
    copied_bytes: u64,
    _snapshot_directory: Option<TempDir>,
    _live_authority_handle: Option<File>,
    _scratch: Arc<SqliteRouteScratch>,
    snapshot_activity: Option<SqliteSourceSnapshotActivity>,
    snapshot_context: Arc<SqliteSourceSnapshotContext>,
    terminal_fence_slot: Arc<SqliteSourceTerminalFenceSlot>,
    explicitly_completed: bool,
    #[cfg(any(test, feature = "test-support"))]
    fail_next_cleanup: bool,
}

impl SqliteSourceReadSnapshot {
    pub fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        verify_snapshot_active(connection)?;
        Ok(connection)
    }

    pub fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    pub fn admitted_revision_is_replay_safe(&self) -> bool {
        self.admitted_revision_is_replay_safe
    }

    /// Retains a content-free terminal revalidator before ownership of this
    /// snapshot is passed to a scanner that closes it through [`Self::finish`].
    ///
    /// The callback fails closed until the snapshot has sealed successfully.
    pub fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> SqliteSourceAccessResult<()> + Send + Sync + 'static> {
        let slot = Arc::clone(&self.terminal_fence_slot);
        Box::new(move || slot.revalidate())
    }

    pub fn strategy(&self) -> SqliteSourceSnapshotStrategy {
        self.strategy
    }

    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn family_revalidation_count(&self) -> u32 {
        self.family
            .as_ref()
            .map(SqliteSourceFamily::revalidation_count)
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot_directory(&self) -> Option<&Path> {
        self._snapshot_directory
            .as_ref()
            .map(tempfile::TempDir::path)
    }

    /// Revalidates the pinned SQLite view and retained DB family without
    /// ending the read transaction.
    pub fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let connection = self.connection()?;
        let current_sqlite_evidence = capture_sqlite_evidence(connection)?;
        if current_sqlite_evidence != self.sqlite_evidence {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let family = self
            .family
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        match self.policy {
            SqliteSourceSnapshotPolicy::ExactRevision => family.revalidate(&self.native_evidence),
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => {
                family.revalidate_database_identity(&self.native_evidence)
            }
            SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                family.revalidate_database_identity(&self.native_evidence)
            }
        }
    }

    /// Ends this read snapshot and retains its exact physical source-family
    /// authority for cheap commit-time revalidation.
    pub fn seal(mut self) -> SqliteSourceAccessResult<SqliteSourceTerminalFence> {
        self.explicitly_completed = true;
        if let Err(error) = self.revalidate() {
            return match self.cleanup_snapshot_storage() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                    primary: Box::new(error),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        let family = match self.family.take() {
            Some(family) => family,
            None => {
                let error = SqliteSourceAccessError::SnapshotNotActive;
                return match self.cleanup_snapshot_storage() {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                        primary: Box::new(error),
                        cleanup: Box::new(cleanup),
                    }),
                };
            }
        };
        let approved_parent_path = family.approved_parent_path().to_path_buf();
        let database_name = family.database_name().to_os_string();
        let data_root = self.snapshot_context.data_root.clone();
        drop(family);
        self.cleanup_snapshot_storage()?;
        let fence = SqliteSourceTerminalFence {
            inner: Arc::new(SqliteSourceTerminalFenceInner {
                data_root,
                approved_parent_path,
                database_name,
                native_evidence: self.native_evidence.clone(),
                evidence: self.evidence.clone(),
                policy: self.policy,
                snapshot_context: Arc::clone(&self.snapshot_context),
            }),
        };
        fence.revalidate()?;
        self.terminal_fence_slot.install(fence.clone())?;
        self.snapshot_context.record_terminal_fence()?;
        Ok(fence)
    }

    fn cleanup_snapshot_storage(&mut self) -> SqliteSourceAccessResult<()> {
        let artifact = if self._snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::ProviderDatabase
        };
        #[cfg(any(test, feature = "test-support"))]
        if std::mem::take(&mut self.fail_next_cleanup) {
            let path = self._snapshot_directory.as_ref().map_or_else(
                || PathBuf::from("<injected-snapshot-cleanup>"),
                |directory| directory.path().to_path_buf(),
            );
            return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "removing a ctx-owned SQLite snapshot directory",
                path,
                source: std::io::Error::other("injected SQLite snapshot cleanup failure"),
            }
            .with_diagnostic(
                SqliteFailurePhase::Cleanup,
                artifact,
                0,
                0,
                SqliteCleanupStatus::Failed,
            ));
        }
        let close_connection = self.connection.take().map_or(Ok(()), |connection| {
            close_snapshot_read_connection(connection, artifact)
        });
        let close_directory = self._snapshot_directory.take().map_or(Ok(()), |directory| {
            snapshot::close_private_snapshot_directory(directory, artifact, 0, 0)
        });
        drop(self.snapshot_activity.take());
        combine_sqlite_source_cleanup(close_connection, close_directory)
    }

    /// Compatibility path for callers that need only closing evidence.
    ///
    /// New shared lifecycles should keep the fence returned by [`Self::seal`]
    /// through commit-time physical revalidation.
    pub fn finish(self) -> SqliteSourceAccessResult<SqliteSourceEvidence> {
        let fence = self.seal()?;
        Ok(fence.evidence().clone())
    }

    /// Completes a provider operation and always seals the physical snapshot,
    /// preserving both failures when the operation and finalization fail.
    pub fn finish_with<T, E>(
        self,
        primary: std::result::Result<T, E>,
    ) -> std::result::Result<T, crate::SqliteReadFinalizationError<E, SqliteSourceAccessError>>
    {
        crate::sqlite::combine_sqlite_read_finalization(primary, self.finish().map(|_| ()))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot_counters(&self) -> SqliteSourceSnapshotCounters {
        self.snapshot_context.snapshot()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn counter_observer(&self) -> SqliteSourceSnapshotCounterObserver {
        SqliteSourceSnapshotCounterObserver {
            context: Arc::clone(&self.snapshot_context),
        }
    }
}

impl Drop for SqliteSourceReadSnapshot {
    fn drop(&mut self) {
        if !self.explicitly_completed {
            self.snapshot_context.record_unfinished_drop();
        }
        if let Err(error) = self.cleanup_snapshot_storage() {
            eprintln!("ctx SQLite snapshot fallback cleanup failed: {error}");
        }
    }
}

fn close_snapshot_read_connection(
    connection: Connection,
    artifact: SqliteArtifactKind,
) -> SqliteSourceAccessResult<()> {
    let clear = clear_snapshot_authorizer(&connection).map_err(|source| {
        SqliteSourceAccessError::CleanupUnavailable {
            operation: "clearing the SQLite snapshot authorizer",
            source: Box::new(source),
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    });
    let rollback = connection.execute_batch("ROLLBACK").map_err(|source| {
        SqliteSourceAccessError::ScratchSqliteUnavailable {
            operation: "ending the private SQLite read snapshot",
            source,
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            0,
            0,
            SqliteCleanupStatus::Failed,
        )
    });
    let close = snapshot::close_private_sqlite_connection(
        connection,
        "closing the private SQLite read snapshot",
        artifact,
        0,
        0,
    );
    let result = combine_sqlite_source_cleanup(clear, rollback);
    combine_sqlite_source_cleanup(result, close)
}

fn revalidate_live_database_schema(
    family: &SqliteSourceFamily,
    native_evidence: &SqliteFamilyEvidence,
    expected_schema: &SqliteSchemaEvidence,
) -> SqliteSourceAccessResult<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (family, native_evidence, expected_schema);
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "pinned read-only WAL snapshots require the Linux unix VFS".to_owned(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        family.revalidate_database_identity(native_evidence)?;
        let (connection, _authority_handle) =
            snapshot::acquisition::open_pinned_read_only_wal(family)
                .map_err(map_revalidation_error)?;
        let validation = (|| {
            verify_connection_read_only(&connection)?;
            configure_and_pin_snapshot(&connection)?;
            let current = capture_sqlite_evidence(&connection)?;
            if current.schema() != expected_schema {
                return Err(SqliteSourceAccessError::SourceChanged);
            }
            family.revalidate_database_identity(native_evidence)
        })();
        let cleanup =
            close_snapshot_read_connection(connection, SqliteArtifactKind::ProviderDatabase);
        combine_sqlite_source_cleanup(validation, cleanup).map_err(map_revalidation_error)
    }
}

fn combine_sqlite_source_cleanup(
    primary: SqliteSourceAccessResult<()>,
    cleanup: SqliteSourceAccessResult<()>,
) -> SqliteSourceAccessResult<()> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(SqliteSourceAccessError::Finalization {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}

mod family;
mod snapshot;

use family::{
    capture_sqlite_evidence, clear_snapshot_authorizer, configure_and_pin_snapshot,
    map_provider_source_error, map_provider_source_revalidation_error, map_revalidation_error,
    map_revalidation_io_error, sqlite_error, validate_approved_parent_path,
    verify_connection_read_only, verify_snapshot_active, ExpectedObjectKind, NativeFileIdentity,
    NativeFileState, SqliteConnectionEvidence, SqliteFamilyEvidence, SqliteFamilyMember,
    SqliteSchemaEvidence, SqliteSnapshotEvidence, SqliteSourceFamily,
};
#[cfg(any(test, feature = "test-support"))]
pub use snapshot::{
    fail_next_opened_snapshot_cleanup_for_test, fail_next_private_directory_cleanup_for_test,
    force_next_pinned_wal_unavailable_for_test,
};
pub use snapshot::{
    open_root_handle_sqlite_source_snapshot, open_root_handle_sqlite_source_snapshot_with_limits,
    retain_sqlite_source_directory_authority, SqliteSourceSnapshotLimits,
};

#[cfg(any(test, feature = "test-support"))]
mod tests;
