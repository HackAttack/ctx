//! Neutral, verifier-driven publication of the fixed managed Core/companion pair.
//!
//! This module owns only local transaction mechanics. It does not acquire
//! artifacts, understand companion commands, or hold release credentials.

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process;
#[cfg(unix)]
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

mod attempt;
mod filesystem;
mod journal;
mod uninstall;

use attempt::TerminalOutcome;
use filesystem::{FileStamp, Layout, Slot};
use journal::{Journal, Phase};

pub const MANAGED_PAIR_ENVELOPE_RELATIVE_PATH: &str = "share/ctx/managed-pair-envelope.json";
pub const MANAGED_PAIR_STATE_RELATIVE_PATH: &str = "share/ctx/managed-pair-state.json";

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_COMPONENT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENVELOPE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 64 * 1024;

/// The fixed release target returned by a trusted signed-envelope verifier.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedPairTarget {
    LinuxArm64,
    LinuxX64,
    MacosArm64,
    MacosX64,
    WindowsX64,
}

/// Exact bytes for one neutral managed-pair component.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedPairComponentIdentity {
    sha256: String,
    size_bytes: u64,
}

impl ManagedPairComponentIdentity {
    pub fn new(sha256: impl Into<String>, size_bytes: u64) -> Result<Self> {
        let identity = Self {
            sha256: sha256.into(),
            size_bytes,
        };
        identity.validate("managed-pair component")?;
        Ok(identity)
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    fn validate(&self, label: &str) -> Result<()> {
        validate_sha256(&self.sha256, label)?;
        if self.size_bytes == 0 || self.size_bytes > MAX_COMPONENT_BYTES {
            bail!("{label} size is outside the managed-pair bound");
        }
        Ok(())
    }
}

/// A trust-neutral identity returned only after a caller verifies the signed
/// envelope with its selected release authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedManagedPairIdentity {
    release_name: String,
    target: ManagedPairTarget,
    rollback_generation: u64,
    manifest_sha256: String,
    core: ManagedPairComponentIdentity,
    companion: ManagedPairComponentIdentity,
}

impl VerifiedManagedPairIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        release_name: impl Into<String>,
        target: ManagedPairTarget,
        rollback_generation: u64,
        manifest_sha256: impl Into<String>,
        core: ManagedPairComponentIdentity,
        companion: ManagedPairComponentIdentity,
    ) -> Result<Self> {
        let identity = Self {
            release_name: release_name.into(),
            target,
            rollback_generation,
            manifest_sha256: manifest_sha256.into(),
            core,
            companion,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn release_name(&self) -> &str {
        &self.release_name
    }

    pub fn target(&self) -> ManagedPairTarget {
        self.target
    }

    pub fn rollback_generation(&self) -> u64 {
        self.rollback_generation
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn core(&self) -> &ManagedPairComponentIdentity {
        &self.core
    }

    pub fn companion(&self) -> &ManagedPairComponentIdentity {
        &self.companion
    }

    fn validate(&self) -> Result<()> {
        if self.release_name.is_empty()
            || self.release_name.len() > 128
            || !self
                .release_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            || !self.release_name.as_bytes()[0].is_ascii_alphanumeric()
        {
            bail!("managed-pair release name is invalid");
        }
        if self.rollback_generation == 0 || self.rollback_generation > 9_007_199_254_740_991 {
            bail!("managed-pair rollback generation is invalid");
        }
        validate_sha256(&self.manifest_sha256, "managed-pair manifest")?;
        self.core.validate("managed-pair Core component")?;
        self.companion
            .validate("managed-pair companion component")?;
        Ok(())
    }
}

/// The only production trust input accepted by the managed-pair engine.
///
/// Implementations must authenticate the detached signature, validate the
/// complete neutral manifest contract, and return its exact target/component
/// identity. The engine deliberately exposes no unsigned constructor or flag.
pub trait ManagedPairVerifier {
    fn verify_signed_envelope(&self, signed_envelope: &[u8])
        -> Result<VerifiedManagedPairIdentity>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPairPrepared {
    attempt_id: String,
    identity: VerifiedManagedPairIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPairAttempt {
    attempt_id: String,
    candidate_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPairUninstallAttempt {
    attempt_id: String,
}

impl ManagedPairUninstallAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub const fn retry_or_reboot_may_be_required(&self) -> bool {
        cfg!(windows)
    }
}

impl ManagedPairAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn candidate_root(&self) -> &Path {
        &self.candidate_root
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedPairTransactionStatus {
    Absent,
    Begun,
    Staging,
    Staged,
    Deferred,
    Activating,
    Committed,
    Aborted,
    Failed,
    RollingBack,
}

impl ManagedPairPrepared {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn identity(&self) -> &VerifiedManagedPairIdentity {
        &self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPairActivation {
    Activated,
    PostExitRequired { attempt_id: String, parent_pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPairRecovery {
    None,
    Staged { prepared: ManagedPairPrepared },
    PostExitRequired { attempt_id: String, parent_pid: u32 },
    Activated,
    RolledBack,
}

/// A transaction engine rooted at one installation. Every managed slot is
/// derived from this root; callers cannot supply component or trust-file paths.
#[derive(Debug, Clone)]
pub struct ManagedPairEngine {
    install_root: PathBuf,
}

impl ManagedPairEngine {
    pub fn new(install_root: impl Into<PathBuf>) -> Result<Self> {
        let install_root = install_root.into();
        filesystem::validate_absolute_root(&install_root, "managed-pair install root")?;
        Ok(Self { install_root })
    }

    pub fn install_root(&self) -> &Path {
        &self.install_root
    }

    pub fn begin(&self, verifier: &dyn ManagedPairVerifier) -> Result<ManagedPairAttempt> {
        let layout = Layout::open(&self.install_root, true)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        if let Some(journal) = journal::read(&layout)? {
            recover_for_new_attempt_locked(&layout, journal, verifier)?;
        }
        if let Some(attempt_id) = attempt::read_begin(&layout)? {
            let candidate_root = if filesystem::candidate_exists(&layout, &attempt_id)? {
                filesystem::candidate_root(&self.install_root, &attempt_id)?
            } else {
                filesystem::create_candidate(&self.install_root, &attempt_id)?
            };
            return Ok(ManagedPairAttempt {
                attempt_id,
                candidate_root,
            });
        }
        let attempt_id = Uuid::new_v4().simple().to_string();
        attempt::write_begin(&layout, &attempt_id)?;
        let candidate_root = filesystem::create_candidate(&self.install_root, &attempt_id)?;
        Ok(ManagedPairAttempt {
            attempt_id,
            candidate_root,
        })
    }

    pub fn stage_attempt(
        &self,
        attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairPrepared> {
        let candidate_root = filesystem::candidate_root(&self.install_root, attempt_id)?;
        let result = self.stage_with_fault(
            &candidate_root,
            verifier,
            Some(attempt_id.to_owned()),
            &|_| {},
        );
        let cleanup: Result<()> = (|| -> Result<()> {
            let layout = Layout::open(&self.install_root, false)?;
            let _lock = filesystem::acquire_lock(&layout)?;
            if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
                filesystem::remove_candidate(&layout, attempt_id)?;
                attempt::remove_begin(&layout, attempt_id)?;
                layout.remove_empty_candidate_base()?;
            }
            if result.is_err() && journal::read(&layout)?.is_none() {
                attempt::write_terminal(
                    &layout,
                    attempt_id,
                    TerminalOutcome::Failed,
                    Some("staging_failed"),
                )?;
            }
            Ok(())
        })();
        match (result, cleanup) {
            (Ok(prepared), Ok(())) => Ok(prepared),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error).context("clean managed-pair candidate"),
            (Err(error), Err(cleanup)) => Err(anyhow!(
                "stage managed pair: {error:#}; candidate cleanup failed: {cleanup:#}"
            )),
        }
    }

    pub fn status(&self, attempt_id: &str) -> Result<ManagedPairTransactionStatus> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair attempt ID is invalid");
        }
        match std::fs::symlink_metadata(&self.install_root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ManagedPairTransactionStatus::Absent);
            }
            Err(error) => return Err(error).context("inspect managed-pair install root"),
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if let Some(journal) = journal::read(&layout)? {
            if journal.attempt_id != attempt_id {
                bail!("a different managed-pair transaction is active");
            }
            return Ok(match journal.phase() {
                Phase::Staging => ManagedPairTransactionStatus::Staging,
                Phase::Staged => ManagedPairTransactionStatus::Staged,
                Phase::Deferred => ManagedPairTransactionStatus::Deferred,
                Phase::Activating => ManagedPairTransactionStatus::Activating,
                Phase::Committed => ManagedPairTransactionStatus::Committed,
                Phase::RollingBack => ManagedPairTransactionStatus::RollingBack,
            });
        }
        if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
            return Ok(ManagedPairTransactionStatus::Begun);
        }
        Ok(match attempt::read_terminal(&layout)? {
            Some(receipt) if receipt.attempt_id == attempt_id => match receipt.outcome {
                TerminalOutcome::Committed => ManagedPairTransactionStatus::Committed,
                TerminalOutcome::Aborted => ManagedPairTransactionStatus::Aborted,
                TerminalOutcome::Failed => ManagedPairTransactionStatus::Failed,
            },
            _ => ManagedPairTransactionStatus::Absent,
        })
    }

    pub fn abort(&self, attempt_id: &str) -> Result<bool> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair attempt ID is invalid");
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        let mut changed = false;
        if let Some(mut journal) = journal::read(&layout)? {
            if journal.attempt_id != attempt_id {
                bail!("a different managed-pair transaction is active");
            }
            if matches!(journal.phase(), Phase::Activating | Phase::Committed) {
                bail!("managed-pair activation can no longer be aborted");
            }
            rollback(&layout, &mut journal)?;
            attempt::write_terminal(
                &layout,
                attempt_id,
                TerminalOutcome::Aborted,
                Some("aborted"),
            )?;
            changed = true;
        }
        if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
            filesystem::remove_candidate(&layout, attempt_id)?;
            attempt::remove_begin(&layout, attempt_id)?;
            layout.remove_empty_candidate_base()?;
            attempt::write_terminal(
                &layout,
                attempt_id,
                TerminalOutcome::Aborted,
                Some("aborted"),
            )?;
            changed = true;
        }
        Ok(changed)
    }

    pub fn prepare_uninstall(
        &self,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairUninstallAttempt> {
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        Ok(ManagedPairUninstallAttempt {
            attempt_id: uninstall::prepare(&layout, verifier)?,
        })
    }

    pub fn run_post_exit_uninstall_after_parent_exit(
        &self,
        attempt_id: &str,
        parent_pid: u32,
        parent_creation_time: Option<u64>,
    ) -> Result<bool> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair uninstall attempt ID is invalid");
        }
        #[cfg(windows)]
        {
            let creation = parent_creation_time
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("managed-pair parent creation identity is absent"))?;
            filesystem::wait_for_parent_exit(parent_pid, creation)?;
        }
        #[cfg(unix)]
        {
            let _ = parent_creation_time;
            wait_for_unix_parent_exit(parent_pid)?;
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        uninstall::execute(&layout, attempt_id)
    }

    /// Stages the two fixed components, the exact signed envelope, and a
    /// deterministic local state marker without changing active files.
    pub fn stage(
        &self,
        candidate_root: &Path,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairPrepared> {
        self.stage_with_fault(candidate_root, verifier, None, &|_| {})
    }

    /// Activates directly on Unix. Windows records a durable deferred request;
    /// the caller must launch a helper that invokes `run_post_exit_swapper`.
    pub fn activate(
        &self,
        prepared: &ManagedPairPrepared,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairActivation> {
        let layout = Layout::open(&self.install_root, false)?;
        #[cfg(windows)]
        let _ = verifier;
        #[cfg(windows)]
        let _lock = filesystem::acquire_lock(&layout)?;
        #[allow(unused_mut)]
        let mut journal = journal::read(&layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before activation"))?;
        require_prepared(&journal, prepared)?;
        match journal.phase {
            Phase::Staged => {}
            Phase::Deferred if cfg!(windows) => {
                let parent_pid = journal
                    .parent_pid
                    .ok_or_else(|| anyhow!("deferred managed-pair transaction has no parent"))?;
                return Ok(ManagedPairActivation::PostExitRequired {
                    attempt_id: journal.attempt_id,
                    parent_pid,
                });
            }
            _ => bail!("managed-pair transaction is not ready for activation"),
        }

        #[cfg(windows)]
        {
            journal.phase = Phase::Deferred;
            journal.parent_pid = Some(process::id());
            journal.parent_creation_time = Some(filesystem::current_process_creation_identity()?);
            journal::write(&layout, &mut journal)?;
            return Ok(ManagedPairActivation::PostExitRequired {
                attempt_id: journal.attempt_id,
                parent_pid: process::id(),
            });
        }

        #[cfg(not(windows))]
        {
            self.commit(&layout, &journal.attempt_id, verifier, &|_| {})?;
            Ok(ManagedPairActivation::Activated)
        }
    }

    /// Entry point for a separately launched post-exit swapper. It reopens the
    /// durable journal and independently invokes the verifier before commit.
    pub fn run_post_exit_swapper(&self, verifier: &dyn ManagedPairVerifier) -> Result<()> {
        let layout = Layout::open(&self.install_root, false)?;
        let journal = journal::read(&layout)?
            .ok_or_else(|| anyhow!("managed-pair post-exit transaction is absent"))?;
        let attempt_id = journal.attempt_id.clone();
        #[cfg(windows)]
        {
            if journal.phase != Phase::Staged
                && journal.phase != Phase::Deferred
                && journal.phase != Phase::Activating
            {
                bail!("Windows managed-pair transaction is not deferred");
            }
            if journal.phase == Phase::Deferred {
                let parent_pid = journal
                    .parent_pid
                    .ok_or_else(|| anyhow!("Windows managed-pair transaction has no parent PID"))?;
                let parent_creation_time = journal.parent_creation_time.ok_or_else(|| {
                    anyhow!("Windows managed-pair transaction has no parent creation identity")
                })?;
                filesystem::wait_for_parent_exit(parent_pid, parent_creation_time)?;
            }
        }
        #[cfg(not(windows))]
        if journal.phase != Phase::Staged && journal.phase != Phase::Activating {
            bail!("managed-pair transaction is not staged for post-exit activation");
        }
        self.commit(&layout, &attempt_id, verifier, &|_| {})
    }

    pub fn run_post_exit_swapper_after_parent_exit(
        &self,
        attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
        parent_pid: u32,
        parent_creation_time: Option<u64>,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            let creation = parent_creation_time
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("managed-pair parent creation identity is absent"))?;
            filesystem::wait_for_parent_exit(parent_pid, creation)?;
        }
        #[cfg(unix)]
        {
            let _ = parent_creation_time;
            wait_for_unix_parent_exit(parent_pid)?;
        }
        let layout = Layout::open(&self.install_root, false)?;
        self.commit(&layout, attempt_id, verifier, &|_| {})
    }

    /// Resolves an interrupted operation idempotently. A visible new state
    /// marker commits the complete pair; every earlier interruption rolls back.
    pub fn resume(&self, verifier: &dyn ManagedPairVerifier) -> Result<ManagedPairRecovery> {
        let layout = match Layout::open(&self.install_root, false) {
            Ok(layout) => layout,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(ManagedPairRecovery::None)
            }
            Err(error) => return Err(error),
        };
        let _lock = filesystem::acquire_lock(&layout)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        let Some(mut journal) = journal::read(&layout)? else {
            return Ok(ManagedPairRecovery::None);
        };
        journal.validate_for(&layout)?;
        match journal.phase {
            Phase::Staging | Phase::RollingBack => {
                rollback(&layout, &mut journal)?;
                Ok(ManagedPairRecovery::RolledBack)
            }
            Phase::Staged => {
                verify_staged(&layout, &journal, verifier)?;
                Ok(ManagedPairRecovery::Staged {
                    prepared: ManagedPairPrepared {
                        attempt_id: journal.attempt_id,
                        identity: journal.identity,
                    },
                })
            }
            Phase::Deferred => {
                verify_staged(&layout, &journal, verifier)?;
                Ok(ManagedPairRecovery::PostExitRequired {
                    attempt_id: journal.attempt_id,
                    parent_pid: journal.parent_pid.ok_or_else(|| {
                        anyhow!("deferred managed-pair transaction has no parent PID")
                    })?,
                })
            }
            Phase::Activating => {
                if active_matches_staged(&layout, &journal, verifier)? {
                    journal.phase = Phase::Committed;
                    journal::write(&layout, &mut journal)?;
                    finish_committed(&layout, &journal)?;
                    Ok(ManagedPairRecovery::Activated)
                } else {
                    rollback(&layout, &mut journal)?;
                    Ok(ManagedPairRecovery::RolledBack)
                }
            }
            Phase::Committed => {
                validate_active(&layout, verifier)?;
                finish_committed(&layout, &journal)?;
                Ok(ManagedPairRecovery::Activated)
            }
        }
    }

    /// Independently verifies all four active fixed slots.
    pub fn validate_active(
        &self,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<VerifiedManagedPairIdentity> {
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        validate_active(&layout, verifier)
    }

    fn stage_with_fault(
        &self,
        candidate_root: &Path,
        verifier: &dyn ManagedPairVerifier,
        fixed_attempt_id: Option<String>,
        fault: &dyn Fn(&str),
    ) -> Result<ManagedPairPrepared> {
        filesystem::validate_absolute_root(candidate_root, "managed-pair candidate root")?;
        let layout = Layout::open(&self.install_root, true)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        if let Some(expected_attempt) = fixed_attempt_id.as_deref() {
            if attempt::read_begin(&layout)?.as_deref() != Some(expected_attempt) {
                bail!("managed-pair stage attempt is not the active Core-generated attempt");
            }
        }
        let candidate = Layout::open_candidate(candidate_root)?;
        layout.revalidate()?;
        candidate.revalidate()?;
        if journal::read(&layout)?.is_some() {
            bail!("an interrupted managed-pair transaction must be resumed first");
        }

        let envelope = filesystem::read_regular(
            &candidate.target(Slot::Envelope),
            MAX_ENVELOPE_BYTES,
            "managed-pair signed envelope",
        )?;
        candidate.revalidate()?;
        let identity = verifier
            .verify_signed_envelope(&envelope.bytes)
            .context("verify managed-pair signed envelope")?;
        validate_verified_identity(&identity)?;
        validate_retained_pair(&layout, verifier, &identity)?;

        let state = ManagedPairState::new(identity.clone(), &envelope);
        let state_bytes = state.to_bytes()?;
        if state_bytes.len() as u64 > MAX_STATE_BYTES {
            bail!("managed-pair state exceeds its bound");
        }
        let attempt_id = fixed_attempt_id.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let mut journal = Journal::new(
            attempt_id.clone(),
            identity.clone(),
            envelope.stamp.sha256.clone(),
            envelope.stamp.size_bytes,
        );
        for slot in Slot::ALL {
            journal.original[slot.index()] =
                filesystem::stamp_optional(&layout.target(slot), max_bytes(slot), slot.label())?;
            filesystem::require_absent(&layout.staged(slot, &attempt_id), slot.label())?;
            filesystem::require_absent(&layout.backup(slot, &attempt_id), slot.label())?;
        }
        journal::write_initial(&layout, &mut journal)?;
        fault("journal");

        let staged_result = (|| {
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Core.index()] = Some(filesystem::copy_verified(
                &candidate.target(Slot::Core),
                &layout.staged(Slot::Core, &attempt_id),
                identity.core(),
                true,
                Slot::Core.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_core");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Companion.index()] = Some(filesystem::copy_verified(
                &candidate.target(Slot::Companion),
                &layout.staged(Slot::Companion, &attempt_id),
                identity.companion(),
                true,
                Slot::Companion.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_companion");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Envelope.index()] = Some(filesystem::write_new(
                &layout.staged(Slot::Envelope, &attempt_id),
                &envelope.bytes,
                false,
                Slot::Envelope.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_envelope");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::State.index()] = Some(filesystem::write_new(
                &layout.staged(Slot::State, &attempt_id),
                &state_bytes,
                false,
                Slot::State.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_state");
            journal.phase = Phase::Staged;
            journal::write(&layout, &mut journal)?;
            layout.revalidate()?;
            candidate.revalidate()
        })();
        if let Err(error) = staged_result {
            let rollback_error = rollback(&layout, &mut journal).err();
            return match rollback_error {
                Some(rollback_error) => Err(anyhow!(
                    "stage managed pair: {error:#}; rollback failed: {rollback_error:#}"
                )),
                None => Err(error),
            };
        }
        Ok(ManagedPairPrepared {
            attempt_id,
            identity,
        })
    }

    fn commit(
        &self,
        layout: &Layout,
        expected_attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
        fault: &dyn Fn(&str),
    ) -> Result<()> {
        let _lock = filesystem::acquire_lock(layout)?;
        if !journal::valid_attempt_id(expected_attempt_id) {
            bail!("managed-pair expected attempt ID is invalid");
        }
        if let Some(receipt) = attempt::read_terminal(layout)? {
            if receipt.attempt_id == expected_attempt_id
                && receipt.outcome == TerminalOutcome::Committed
            {
                return Ok(());
            }
        }
        let mut journal = journal::read(layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before commit"))?;
        if journal.attempt_id != expected_attempt_id {
            bail!("managed-pair transaction changed before commit");
        }
        journal.validate_for(layout)?;
        match journal.phase {
            Phase::Staged | Phase::Deferred => {
                verify_staged(layout, &journal, verifier)?;
                validate_retained_pair(layout, verifier, &journal.identity)?;
                verify_originals(layout, &journal)?;
                journal.phase = Phase::Activating;
                journal::write(layout, &mut journal)?;
                fault("activating");
            }
            Phase::Activating => verify_staged_or_published(layout, &journal)?,
            Phase::Committed => {
                validate_active(layout, verifier)?;
                return finish_committed(layout, &journal);
            }
            Phase::Staging | Phase::RollingBack => {
                bail!("managed-pair transaction is not ready for activation")
            }
        }
        let bound = journal::read(layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before mutation"))?;
        if bound.attempt_id != expected_attempt_id || bound.phase != Phase::Activating {
            bail!("managed-pair expected attempt changed immediately before mutation");
        }
        journal = bound;

        let result = (|| {
            for slot in Slot::BACKUP_ORDER {
                layout.revalidate()?;
                if let Some(expected) = journal.original[slot.index()].as_ref() {
                    let backup = match filesystem::stamp_optional(
                        &layout.backup(slot, &journal.attempt_id),
                        max_bytes(slot),
                        slot.label(),
                    )? {
                        Some(actual) => {
                            require_same_content(&actual, expected, slot.label())?;
                            actual
                        }
                        None => filesystem::copy_exact(
                            &layout.target(slot),
                            &layout.backup(slot, &journal.attempt_id),
                            expected,
                            max_bytes(slot),
                            matches!(slot, Slot::Core | Slot::Companion),
                            slot.label(),
                        )?,
                    };
                    journal.backups[slot.index()] = Some(backup);
                    journal::write(layout, &mut journal)?;
                }
                fault(slot.backup_fault());
                layout.revalidate()?;
            }
            for slot in Slot::ALL {
                layout.revalidate()?;
                let expected = journal.staged[slot.index()].as_ref().ok_or_else(|| {
                    anyhow!("managed-pair journal has no staged {}", slot.label())
                })?;
                if filesystem::matches_stamp(
                    &layout.target(slot),
                    expected,
                    max_bytes(slot),
                    slot.label(),
                )? {
                    filesystem::require_absent(
                        &layout.staged(slot, &journal.attempt_id),
                        slot.label(),
                    )?;
                } else if journal.original[slot.index()].is_some() {
                    filesystem::durable_replace(
                        &layout.staged(slot, &journal.attempt_id),
                        &layout.target(slot),
                        expected,
                        max_bytes(slot),
                        slot.label(),
                    )?;
                } else {
                    filesystem::rename_exact(
                        &layout.staged(slot, &journal.attempt_id),
                        &layout.target(slot),
                        expected,
                        max_bytes(slot),
                        slot.label(),
                    )?;
                }
                fault(slot.publish_fault());
                layout.revalidate()?;
            }
            Ok(())
        })();

        if let Err(error) = result {
            if active_matches_staged(layout, &journal, verifier)? {
                journal.phase = Phase::Committed;
                journal::write(layout, &mut journal)?;
                finish_committed(layout, &journal)?;
                return Ok(());
            }
            let rollback_error = rollback(layout, &mut journal).err();
            return match rollback_error {
                Some(rollback_error) => Err(anyhow!(
                    "activate managed pair: {error:#}; rollback failed: {rollback_error:#}"
                )),
                None => Err(error),
            };
        }

        validate_active(layout, verifier)?;
        journal.phase = Phase::Committed;
        journal::write(layout, &mut journal)?;
        finish_committed(layout, &journal)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ManagedPairState {
    contract: String,
    schema_version: u32,
    identity: VerifiedManagedPairIdentity,
    envelope_sha256: String,
    envelope_size_bytes: u64,
}

impl ManagedPairState {
    fn new(identity: VerifiedManagedPairIdentity, envelope: &filesystem::ObservedFile) -> Self {
        Self {
            contract: "ctx-managed-pair-state".to_owned(),
            schema_version: STATE_SCHEMA_VERSION,
            identity,
            envelope_sha256: envelope.stamp.sha256.clone(),
            envelope_size_bytes: envelope.stamp.size_bytes,
        }
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self) -> Result<()> {
        if self.contract != "ctx-managed-pair-state"
            || self.schema_version != STATE_SCHEMA_VERSION
            || self.envelope_size_bytes == 0
            || self.envelope_size_bytes > MAX_ENVELOPE_BYTES
        {
            bail!("managed-pair state contract is invalid");
        }
        self.identity.validate()?;
        validate_sha256(&self.envelope_sha256, "managed-pair envelope")
    }
}

fn validate_verified_identity(identity: &VerifiedManagedPairIdentity) -> Result<()> {
    identity.validate()?;
    if identity.target != current_target()? {
        bail!("managed-pair envelope target does not match this executable");
    }
    Ok(())
}

fn current_target() -> Result<ManagedPairTarget> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Ok(ManagedPairTarget::LinuxArm64),
        ("linux", "x86_64") => Ok(ManagedPairTarget::LinuxX64),
        ("macos", "aarch64") => Ok(ManagedPairTarget::MacosArm64),
        ("macos", "x86_64") => Ok(ManagedPairTarget::MacosX64),
        ("windows", "x86_64") => Ok(ManagedPairTarget::WindowsX64),
        (os, arch) => bail!("managed-pair transactions are unsupported on {os}-{arch}"),
    }
}

fn validate_retained_pair(
    layout: &Layout,
    verifier: &dyn ManagedPairVerifier,
    candidate: &VerifiedManagedPairIdentity,
) -> Result<()> {
    layout.revalidate()?;
    let state = filesystem::stamp_optional(
        &layout.target(Slot::State),
        MAX_STATE_BYTES,
        Slot::State.label(),
    )?;
    if state.is_none() {
        if filesystem::stamp_optional(
            &layout.target(Slot::Companion),
            MAX_COMPONENT_BYTES,
            Slot::Companion.label(),
        )?
        .is_some()
            || filesystem::stamp_optional(
                &layout.target(Slot::Envelope),
                MAX_ENVELOPE_BYTES,
                Slot::Envelope.label(),
            )?
            .is_some()
        {
            bail!("refusing a partially active managed pair without its state marker");
        }
        return Ok(());
    }
    let retained = validate_active(layout, verifier)?;
    if candidate.rollback_generation < retained.rollback_generation {
        bail!("managed-pair rollback generation would downgrade the installation");
    }
    if candidate.rollback_generation == retained.rollback_generation && candidate != &retained {
        bail!("managed-pair identity changed without advancing rollback generation");
    }
    Ok(())
}

fn validate_active(
    layout: &Layout,
    verifier: &dyn ManagedPairVerifier,
) -> Result<VerifiedManagedPairIdentity> {
    layout.revalidate()?;
    let state_file = filesystem::read_regular(
        &layout.target(Slot::State),
        MAX_STATE_BYTES,
        Slot::State.label(),
    )?;
    let state: ManagedPairState =
        serde_json::from_slice(&state_file.bytes).context("parse managed-pair state")?;
    state.validate()?;
    validate_verified_identity(&state.identity)?;

    let envelope = filesystem::read_regular(
        &layout.target(Slot::Envelope),
        MAX_ENVELOPE_BYTES,
        Slot::Envelope.label(),
    )?;
    if envelope.stamp.sha256 != state.envelope_sha256
        || envelope.stamp.size_bytes != state.envelope_size_bytes
    {
        bail!("active managed-pair envelope does not match state");
    }
    let verified = verifier
        .verify_signed_envelope(&envelope.bytes)
        .context("reverify active managed-pair envelope")?;
    validate_verified_identity(&verified)?;
    if verified != state.identity {
        bail!("active managed-pair state does not match its signed envelope");
    }
    filesystem::verify_content(
        &layout.target(Slot::Core),
        verified.core(),
        Slot::Core.label(),
    )?;
    filesystem::verify_content(
        &layout.target(Slot::Companion),
        verified.companion(),
        Slot::Companion.label(),
    )?;
    layout.revalidate()?;
    Ok(verified)
}

fn verify_staged(
    layout: &Layout,
    journal: &Journal,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    layout.revalidate()?;
    journal.validate_for(layout)?;
    for slot in Slot::ALL {
        let expected = journal.staged[slot.index()]
            .as_ref()
            .ok_or_else(|| anyhow!("managed-pair journal has no staged {}", slot.label()))?;
        filesystem::require_stamp(
            &layout.staged(slot, &journal.attempt_id),
            expected,
            max_bytes(slot),
            slot.label(),
        )?;
    }
    let envelope = filesystem::read_regular(
        &layout.staged(Slot::Envelope, &journal.attempt_id),
        MAX_ENVELOPE_BYTES,
        Slot::Envelope.label(),
    )?;
    let verified = verifier
        .verify_signed_envelope(&envelope.bytes)
        .context("reverify staged managed-pair envelope")?;
    validate_verified_identity(&verified)?;
    if verified != journal.identity
        || envelope.stamp.sha256 != journal.envelope_sha256
        || envelope.stamp.size_bytes != journal.envelope_size_bytes
    {
        bail!("staged managed-pair envelope identity changed after staging");
    }
    filesystem::verify_content(
        &layout.staged(Slot::Core, &journal.attempt_id),
        verified.core(),
        Slot::Core.label(),
    )?;
    filesystem::verify_content(
        &layout.staged(Slot::Companion, &journal.attempt_id),
        verified.companion(),
        Slot::Companion.label(),
    )?;
    let expected_state = ManagedPairState {
        contract: "ctx-managed-pair-state".to_owned(),
        schema_version: STATE_SCHEMA_VERSION,
        identity: verified,
        envelope_sha256: envelope.stamp.sha256,
        envelope_size_bytes: envelope.stamp.size_bytes,
    }
    .to_bytes()?;
    let state = filesystem::read_regular(
        &layout.staged(Slot::State, &journal.attempt_id),
        MAX_STATE_BYTES,
        Slot::State.label(),
    )?;
    if state.bytes != expected_state {
        bail!("staged managed-pair state is not the verified deterministic marker");
    }
    layout.revalidate()
}

fn verify_staged_or_published(layout: &Layout, journal: &Journal) -> Result<()> {
    layout.revalidate()?;
    for slot in Slot::ALL {
        let expected = journal.staged[slot.index()]
            .as_ref()
            .ok_or_else(|| anyhow!("managed-pair journal has no staged {}", slot.label()))?;
        let staged_matches = filesystem::matches_stamp(
            &layout.staged(slot, &journal.attempt_id),
            expected,
            max_bytes(slot),
            slot.label(),
        )?;
        let target_matches = filesystem::matches_stamp(
            &layout.target(slot),
            expected,
            max_bytes(slot),
            slot.label(),
        )?;
        if staged_matches == target_matches {
            bail!(
                "managed-pair activation has missing or duplicate {}",
                slot.label()
            );
        }
    }
    layout.revalidate()
}

fn verify_originals(layout: &Layout, journal: &Journal) -> Result<()> {
    layout.revalidate()?;
    for slot in Slot::ALL {
        match journal.original[slot.index()].as_ref() {
            Some(expected) => filesystem::require_stamp(
                &layout.target(slot),
                expected,
                max_bytes(slot),
                slot.label(),
            )?,
            None => filesystem::require_absent(&layout.target(slot), slot.label())?,
        }
        filesystem::require_absent(&layout.backup(slot, &journal.attempt_id), slot.label())?;
    }
    layout.revalidate()
}

fn active_matches_staged(
    layout: &Layout,
    journal: &Journal,
    verifier: &dyn ManagedPairVerifier,
) -> Result<bool> {
    let Some(expected_state) = journal.staged[Slot::State.index()].as_ref() else {
        return Ok(false);
    };
    if !filesystem::matches_stamp(
        &layout.target(Slot::State),
        expected_state,
        MAX_STATE_BYTES,
        Slot::State.label(),
    )? {
        return Ok(false);
    }
    Ok(validate_active(layout, verifier).is_ok())
}

fn rollback(layout: &Layout, journal: &mut Journal) -> Result<()> {
    rollback_with_fault(layout, journal, &|_| {})
}

fn rollback_with_fault(layout: &Layout, journal: &mut Journal, fault: &dyn Fn(&str)) -> Result<()> {
    let allow_unrecorded_staging = journal.phase == Phase::Staging;
    journal.phase = Phase::RollingBack;
    journal::write(layout, journal)?;
    normalize_backups(layout, journal)?;
    layout.revalidate()?;
    hide_state_for_rollback(layout, journal)?;
    fault("rollback_hide_state");
    layout.revalidate()?;

    for slot in Slot::DATA {
        restore_data_slot(layout, journal, slot)?;
        cleanup_staged_slot(layout, journal, slot, allow_unrecorded_staging)?;
        fault(match slot {
            Slot::Core => "rollback_core",
            Slot::Companion => "rollback_companion",
            Slot::Envelope => "rollback_envelope",
            Slot::State => unreachable!(),
        });
        layout.revalidate()?;
    }

    cleanup_staged_slot(layout, journal, Slot::State, allow_unrecorded_staging)?;
    restore_state_last(layout, journal)?;
    fault("rollback_restore_state");
    layout.revalidate()?;
    attempt::write_terminal(
        layout,
        &journal.attempt_id,
        TerminalOutcome::Failed,
        Some("recovered_by_rollback"),
    )?;
    journal::remove(layout, journal)
}

fn hide_state_for_rollback(layout: &Layout, journal: &Journal) -> Result<()> {
    let slot = Slot::State;
    let target = layout.target(slot);
    let backup = layout.backup(slot, &journal.attempt_id);
    let old = journal.original[slot.index()].as_ref();
    let backup_expected = journal.backups[slot.index()].as_ref();
    let new = journal.staged[slot.index()].as_ref();
    let backup_present = filesystem::stamp_optional(&backup, max_bytes(slot), slot.label())?;
    let target_present = filesystem::stamp_optional(&target, max_bytes(slot), slot.label())?;

    match (old, backup_present.as_ref(), target_present.as_ref()) {
        (Some(old), Some(actual_backup), target_stamp)
            if backup_expected == Some(actual_backup) =>
        {
            if let Some(actual_target) = target_stamp {
                if new != Some(actual_target)
                    && require_same_content(actual_target, old, slot.label()).is_err()
                {
                    bail!("refusing a substituted active managed-pair state during rollback");
                }
                filesystem::remove_if_exact(&target, actual_target, max_bytes(slot), slot.label())?;
            }
        }
        (Some(old), None, Some(actual_target))
            if actual_target == old || backup_expected == Some(actual_target) =>
        {
            return Ok(());
        }
        (Some(_), Some(_), _) => {
            bail!("refusing a substituted managed-pair state backup")
        }
        (Some(_), None, _) => {
            bail!("managed-pair original state disappeared during rollback")
        }
        (None, None, Some(actual_target)) if new == Some(actual_target) => {
            filesystem::remove_if_exact(&target, actual_target, max_bytes(slot), slot.label())?;
        }
        (None, None, None) => {}
        (None, Some(_), _) => bail!("unexpected managed-pair state backup exists"),
        (None, None, Some(_)) => {
            bail!("refusing a substituted active managed-pair state during rollback")
        }
    }
    filesystem::require_absent(&target, slot.label())
}

fn restore_data_slot(layout: &Layout, journal: &Journal, slot: Slot) -> Result<()> {
    let target = layout.target(slot);
    let backup = layout.backup(slot, &journal.attempt_id);
    let new = journal.staged[slot.index()].as_ref();
    let old = journal.original[slot.index()].as_ref();
    let backup_expected = journal.backups[slot.index()].as_ref();
    let backup_present = filesystem::stamp_optional(&backup, max_bytes(slot), slot.label())?;
    let target_present = filesystem::stamp_optional(&target, max_bytes(slot), slot.label())?;
    match (old, backup_present.as_ref(), target_present.as_ref()) {
        (Some(old), Some(actual_backup), Some(actual_target))
            if backup_expected == Some(actual_backup) && actual_target == old =>
        {
            filesystem::remove_if_exact(&backup, actual_backup, max_bytes(slot), slot.label())
        }
        (Some(_), Some(actual_backup), Some(actual_target))
            if backup_expected == Some(actual_backup) && new == Some(actual_target) =>
        {
            filesystem::durable_replace(
                &backup,
                &target,
                actual_backup,
                max_bytes(slot),
                slot.label(),
            )
        }
        (Some(_), Some(actual_backup), None) if backup_expected == Some(actual_backup) => {
            filesystem::rename_exact(
                &backup,
                &target,
                actual_backup,
                max_bytes(slot),
                slot.label(),
            )
        }
        (Some(old), None, Some(actual)) if actual == old || backup_expected == Some(actual) => {
            Ok(())
        }
        (Some(_), Some(_), _) => bail!(
            "refusing a substituted managed-pair backup for {}",
            slot.label()
        ),
        (Some(_), None, Some(_)) => {
            bail!(
                "refusing substituted active {} during rollback",
                slot.label()
            )
        }
        (Some(_), None, None) => bail!("managed-pair original {} is absent", slot.label()),
        (None, None, Some(actual)) if new == Some(actual) => {
            filesystem::remove_if_exact(&target, actual, max_bytes(slot), slot.label())
        }
        (None, None, None) => Ok(()),
        (None, Some(_), _) => bail!("unexpected managed-pair backup exists for {}", slot.label()),
        (None, None, Some(_)) => {
            bail!(
                "refusing substituted active {} during rollback",
                slot.label()
            )
        }
    }
}

fn cleanup_staged_slot(
    layout: &Layout,
    journal: &Journal,
    slot: Slot,
    allow_unrecorded_staging: bool,
) -> Result<()> {
    let staged = layout.staged(slot, &journal.attempt_id);
    if let Some(new) = journal.staged[slot.index()].as_ref() {
        filesystem::remove_if_exact(&staged, new, max_bytes(slot), slot.label())
    } else if allow_unrecorded_staging {
        if let Some(actual) = filesystem::stamp_optional(&staged, max_bytes(slot), slot.label())? {
            let (expected_size, expected_sha256) = expected_staged_content(journal, slot)?;
            if actual.size_bytes != expected_size || actual.sha256 != expected_sha256 {
                bail!(
                    "refusing to remove unrecorded substituted staging file for {}",
                    slot.label()
                );
            }
            filesystem::remove_if_exact(&staged, &actual, max_bytes(slot), slot.label())?;
        }
        Ok(())
    } else {
        filesystem::require_absent(&staged, slot.label())
    }
}

fn restore_state_last(layout: &Layout, journal: &Journal) -> Result<()> {
    let slot = Slot::State;
    let target = layout.target(slot);
    let backup = layout.backup(slot, &journal.attempt_id);
    match journal.original[slot.index()].as_ref() {
        Some(old) => {
            if filesystem::stamp_optional(&target, max_bytes(slot), slot.label())?
                .is_some_and(|actual| require_same_content(&actual, old, slot.label()).is_ok())
            {
                filesystem::require_absent(&backup, slot.label())?;
                return Ok(());
            }
            filesystem::require_absent(&target, slot.label())?;
            let expected = journal.backups[slot.index()]
                .as_ref()
                .ok_or_else(|| anyhow!("managed-pair state rollback backup identity is absent"))?;
            filesystem::rename_exact(&backup, &target, expected, max_bytes(slot), slot.label())
        }
        None => {
            filesystem::require_absent(&target, slot.label())?;
            filesystem::require_absent(&backup, slot.label())
        }
    }
}

fn expected_staged_content(journal: &Journal, slot: Slot) -> Result<(u64, String)> {
    match slot {
        Slot::Core => Ok((
            journal.identity.core.size_bytes,
            journal.identity.core.sha256.clone(),
        )),
        Slot::Companion => Ok((
            journal.identity.companion.size_bytes,
            journal.identity.companion.sha256.clone(),
        )),
        Slot::Envelope => Ok((journal.envelope_size_bytes, journal.envelope_sha256.clone())),
        Slot::State => {
            let bytes = ManagedPairState {
                contract: "ctx-managed-pair-state".to_owned(),
                schema_version: STATE_SCHEMA_VERSION,
                identity: journal.identity.clone(),
                envelope_sha256: journal.envelope_sha256.clone(),
                envelope_size_bytes: journal.envelope_size_bytes,
            }
            .to_bytes()?;
            Ok((
                u64::try_from(bytes.len())?,
                format!("{:x}", Sha256::digest(&bytes)),
            ))
        }
    }
}

fn finish_committed(layout: &Layout, journal: &Journal) -> Result<()> {
    layout.revalidate()?;
    for slot in Slot::ALL {
        let expected = journal.staged[slot.index()]
            .as_ref()
            .ok_or_else(|| anyhow!("committed journal has no staged {}", slot.label()))?;
        filesystem::require_stamp(
            &layout.target(slot),
            expected,
            max_bytes(slot),
            slot.label(),
        )?;
        filesystem::require_absent(&layout.staged(slot, &journal.attempt_id), slot.label())?;
        if journal.original[slot.index()].is_some() {
            let backup = journal.backups[slot.index()].as_ref().ok_or_else(|| {
                anyhow!(
                    "committed journal has no rollback backup for {}",
                    slot.label()
                )
            })?;
            filesystem::remove_if_exact(
                &layout.backup(slot, &journal.attempt_id),
                backup,
                max_bytes(slot),
                slot.label(),
            )?;
        } else {
            filesystem::require_absent(&layout.backup(slot, &journal.attempt_id), slot.label())?;
        }
    }
    attempt::write_terminal(
        layout,
        &journal.attempt_id,
        TerminalOutcome::Committed,
        None,
    )?;
    journal::remove(layout, journal)?;
    layout.revalidate()
}

fn normalize_backups(layout: &Layout, journal: &mut Journal) -> Result<()> {
    let mut changed = false;
    for slot in Slot::ALL {
        let actual = filesystem::stamp_optional(
            &layout.backup(slot, &journal.attempt_id),
            max_bytes(slot),
            slot.label(),
        )?;
        match (journal.original[slot.index()].as_ref(), actual) {
            (Some(original), Some(actual)) => {
                require_same_content(&actual, original, slot.label())?;
                if journal.backups[slot.index()].as_ref() != Some(&actual) {
                    journal.backups[slot.index()] = Some(actual);
                    changed = true;
                }
            }
            (Some(original), None) if journal.backups[slot.index()].is_some() => {
                let recorded_backup = journal.backups[slot.index()].as_ref();
                let target = filesystem::stamp_optional(
                    &layout.target(slot),
                    max_bytes(slot),
                    slot.label(),
                )?;
                if target.as_ref() != Some(original) && target.as_ref() != recorded_backup {
                    bail!("recorded managed-pair rollback backup disappeared");
                }
            }
            (Some(_), None) => {}
            (None, Some(_)) => bail!("unexpected managed-pair rollback backup exists"),
            (None, None) => {}
        }
    }
    if changed {
        journal::write(layout, journal)?;
    }
    Ok(())
}

fn require_same_content(actual: &FileStamp, expected: &FileStamp, label: &str) -> Result<()> {
    if actual.size_bytes != expected.size_bytes || actual.sha256 != expected.sha256 {
        bail!("managed-pair rollback backup for {label} has substituted content");
    }
    Ok(())
}

fn require_prepared(journal: &Journal, prepared: &ManagedPairPrepared) -> Result<()> {
    if journal.attempt_id != prepared.attempt_id || journal.identity != prepared.identity {
        bail!("prepared managed-pair handle does not match the durable transaction");
    }
    Ok(())
}

fn recover_for_new_attempt_locked(
    layout: &Layout,
    mut journal: Journal,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    let attempt_id = journal.attempt_id.clone();
    match journal.phase {
        Phase::Committed => {
            validate_active(layout, verifier)?;
            finish_committed(layout, &journal)?;
        }
        Phase::Activating if active_matches_staged(layout, &journal, verifier)? => {
            journal.phase = Phase::Committed;
            journal::write(layout, &mut journal)?;
            finish_committed(layout, &journal)?;
        }
        Phase::Staging
        | Phase::Staged
        | Phase::Deferred
        | Phase::Activating
        | Phase::RollingBack => rollback(layout, &mut journal)?,
    }
    if attempt::read_begin(layout)?.as_deref() == Some(&attempt_id) {
        if filesystem::candidate_exists(layout, &attempt_id)? {
            filesystem::remove_candidate(layout, &attempt_id)?;
        }
        attempt::remove_begin(layout, &attempt_id)?;
        layout.remove_empty_candidate_base()?;
    }
    Ok(())
}

fn max_bytes(slot: Slot) -> u64 {
    match slot {
        Slot::Core | Slot::Companion => MAX_COMPONENT_BYTES,
        Slot::Envelope => MAX_ENVELOPE_BYTES,
        Slot::State => MAX_STATE_BYTES,
    }
}

fn reject_legacy_transaction(install_root: &Path) -> Result<()> {
    if filesystem::legacy_journal_present(install_root)? {
        bail!(
            "legacy managed-pair installer transaction requires installer recovery before runtime upgrade"
        );
    }
    Ok(())
}

#[cfg(unix)]
fn wait_for_unix_parent_exit(parent_pid: u32) -> Result<()> {
    let parent = i32::try_from(parent_pid)
        .ok()
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("managed-pair parent PID is invalid"))?;
    let observed_parent = unsafe { libc::getppid() };
    if observed_parent != parent {
        let status = unsafe { libc::kill(parent, 0) };
        if status == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        bail!("managed-pair helper was not launched by the expected companion process");
    }
    let started = Instant::now();
    loop {
        let status = unsafe { libc::kill(parent, 0) };
        if status == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error).context("wait for managed-pair companion exit");
            }
        }
        if started.elapsed() >= Duration::from_secs(5 * 60) {
            bail!("timed out waiting for the managed-pair companion to exit");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} SHA-256 identity is invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
