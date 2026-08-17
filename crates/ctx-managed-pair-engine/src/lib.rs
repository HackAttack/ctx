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

mod engine;
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
