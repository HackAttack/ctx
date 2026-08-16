use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{filesystem, filesystem::Entry, FileStamp, Layout, VerifiedManagedPairIdentity};

const SCHEMA_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Staging,
    Staged,
    Deferred,
    Activating,
    RollingBack,
    Committed,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    schema_version: u32,
    pub(super) attempt_id: String,
    pub(super) identity: VerifiedManagedPairIdentity,
    pub(super) envelope_sha256: String,
    pub(super) envelope_size_bytes: u64,
    pub(super) phase: Phase,
    pub(super) parent_pid: Option<u32>,
    pub(super) parent_creation_time: Option<u64>,
    pub(super) original: [Option<FileStamp>; 4],
    pub(super) staged: [Option<FileStamp>; 4],
    pub(super) backups: [Option<FileStamp>; 4],
    binding_sha256: String,
}

impl Journal {
    pub(super) fn new(
        attempt_id: String,
        identity: VerifiedManagedPairIdentity,
        envelope_sha256: String,
        envelope_size_bytes: u64,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            attempt_id,
            identity,
            envelope_sha256,
            envelope_size_bytes,
            phase: Phase::Staging,
            parent_pid: None,
            parent_creation_time: None,
            original: std::array::from_fn(|_| None),
            staged: std::array::from_fn(|_| None),
            backups: std::array::from_fn(|_| None),
            binding_sha256: String::new(),
        }
    }

    pub(super) fn validate_for(&self, _layout: &Layout) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || !is_attempt_id(&self.attempt_id)
            || self.envelope_size_bytes == 0
            || self.envelope_size_bytes > super::MAX_ENVELOPE_BYTES
        {
            bail!("managed-pair transaction journal is invalid");
        }
        self.identity.validate()?;
        super::validate_sha256(&self.envelope_sha256, "managed-pair journal envelope")?;
        if self.binding_sha256 != binding(self)? {
            bail!("managed-pair transaction journal binding is invalid");
        }
        match self.phase {
            Phase::Staged | Phase::Deferred | Phase::Activating | Phase::Committed
                if self.staged.iter().any(Option::is_none) =>
            {
                bail!("managed-pair journal is missing staged identities")
            }
            Phase::Deferred if self.parent_pid.is_none() || self.parent_creation_time.is_none() => {
                bail!("deferred managed-pair journal has no complete parent identity")
            }
            _ => {}
        }
        for (original, backup) in self.original.iter().zip(&self.backups) {
            if original.is_none() && backup.is_some() {
                bail!("managed-pair journal has an unexpected rollback backup");
            }
        }
        if self.parent_pid == Some(0)
            || self.parent_creation_time == Some(0)
            || self.parent_pid.is_some() != self.parent_creation_time.is_some()
        {
            bail!("managed-pair journal has an invalid parent identity");
        }
        Ok(())
    }

    pub(super) const fn phase(&self) -> Phase {
        self.phase
    }
}

pub(super) fn valid_attempt_id(value: &str) -> bool {
    is_attempt_id(value)
}

pub(super) fn read(layout: &Layout) -> Result<Option<Journal>> {
    layout.revalidate()?;
    let current = read_path(
        layout,
        &layout.journal(),
        "managed-pair transaction journal",
    )?;
    let temporary_path = layout.journal_temporary();
    let temporary = filesystem::read_temporary(
        &temporary_path,
        MAX_JOURNAL_BYTES,
        "managed-pair journal temporary",
    )?;
    let Some(observed) = temporary else {
        return Ok(current);
    };
    let temporary_journal = match parse_observed(layout, &observed.bytes, "temporary journal") {
        Ok(journal) => journal,
        Err(error) => {
            filesystem::remove_temporary_exact(
                &temporary_path,
                &observed.stamp,
                MAX_JOURNAL_BYTES,
                "incomplete managed-pair journal temporary",
            )?;
            let _ = error;
            return Ok(current);
        }
    };
    if current
        .as_ref()
        .is_some_and(|current| current.attempt_id != temporary_journal.attempt_id)
    {
        bail!("managed-pair temporary journal belongs to another transaction");
    }
    filesystem::durable_replace(
        &temporary_path,
        &layout.journal(),
        &observed.stamp,
        MAX_JOURNAL_BYTES,
        "managed-pair journal temporary",
    )?;
    layout.revalidate()?;
    Ok(Some(temporary_journal))
}

pub(super) fn write_initial(layout: &Layout, journal: &mut Journal) -> Result<()> {
    let bytes = encoded(journal)?;
    let path = layout.journal();
    let temporary = layout.journal_temporary();
    filesystem::require_absent(&path, "managed-pair transaction journal")?;
    filesystem::require_absent(&temporary, "managed-pair journal temporary")?;
    let stamp = filesystem::write_new(&temporary, &bytes, false, "managed-pair journal temporary")?;
    filesystem::durable_replace(
        &temporary,
        &path,
        &stamp,
        MAX_JOURNAL_BYTES,
        "managed-pair journal temporary",
    )?;
    layout.revalidate()?;
    require_written_binding(layout, journal, "created")
}

pub(super) fn write(layout: &Layout, journal: &mut Journal) -> Result<()> {
    let bytes = encoded(journal)?;
    let temporary = layout.journal_temporary();
    filesystem::require_absent(&temporary, "managed-pair journal temporary")?;
    let stamp = filesystem::write_new(&temporary, &bytes, false, "managed-pair journal temporary")?;
    filesystem::durable_replace(
        &temporary,
        &layout.journal(),
        &stamp,
        MAX_JOURNAL_BYTES,
        "managed-pair journal temporary",
    )?;
    layout.revalidate()?;
    require_written_binding(layout, journal, "updated")
}

pub(super) fn remove(layout: &Layout, journal: &Journal) -> Result<()> {
    let path = layout.journal();
    if let Some(current) = read(layout)? {
        if current.attempt_id != journal.attempt_id
            || current.binding_sha256 != journal.binding_sha256
        {
            bail!("refusing to remove a replaced managed-pair journal");
        }
        let stamp = filesystem::stamp_optional(
            &path,
            MAX_JOURNAL_BYTES,
            "managed-pair transaction journal",
        )?
        .ok_or_else(|| anyhow::anyhow!("managed-pair journal disappeared"))?;
        filesystem::remove_if_exact(
            &path,
            &stamp,
            MAX_JOURNAL_BYTES,
            "managed-pair transaction journal",
        )?;
    }
    filesystem::require_absent(
        &layout.journal_temporary(),
        "managed-pair journal temporary",
    )
}

fn require_written_binding(layout: &Layout, journal: &Journal, action: &str) -> Result<()> {
    let reread = read(layout)?.ok_or_else(|| anyhow::anyhow!("managed-pair journal vanished"))?;
    if reread.binding_sha256 != journal.binding_sha256 {
        bail!("managed-pair journal changed while being {action}");
    }
    Ok(())
}

fn read_path(layout: &Layout, path: &Entry, label: &str) -> Result<Option<Journal>> {
    let observed = match filesystem::read_regular(path, MAX_JOURNAL_BYTES, label) {
        Ok(observed) => observed,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    parse_observed(layout, &observed.bytes, label).map(Some)
}

fn parse_observed(layout: &Layout, bytes: &[u8], label: &str) -> Result<Journal> {
    let journal: Journal =
        serde_json::from_slice(bytes).with_context(|| format!("parse managed-pair {label}"))?;
    journal.validate_for(layout)?;
    Ok(journal)
}

fn encoded(journal: &mut Journal) -> Result<Vec<u8>> {
    journal.binding_sha256 = binding(journal)?;
    let mut bytes = serde_json::to_vec_pretty(journal)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        bail!("managed-pair transaction journal exceeds its bound");
    }
    Ok(bytes)
}

fn binding(journal: &Journal) -> Result<String> {
    let mut clone = journal.clone();
    clone.binding_sha256.clear();
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&clone)?)))
}

fn is_attempt_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
pub(super) fn write_temporary_for_test(layout: &Layout, journal: &mut Journal) -> Result<()> {
    let bytes = encoded(journal)?;
    filesystem::require_absent(
        &layout.journal_temporary(),
        "managed-pair journal temporary",
    )?;
    filesystem::write_new(
        &layout.journal_temporary(),
        &bytes,
        false,
        "managed-pair journal temporary",
    )?;
    Ok(())
}
