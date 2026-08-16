use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    filesystem::{self, FileStamp, Layout, Slot},
    journal, validate_active, ManagedPairVerifier, MAX_COMPONENT_BYTES, MAX_ENVELOPE_BYTES,
    MAX_STATE_BYTES,
};

const SCHEMA_VERSION: u32 = 1;
const MAX_UNINSTALL_JOURNAL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UninstallJournal {
    schema_version: u32,
    attempt_id: String,
    original: [FileStamp; 4],
    ancillary_removed: bool,
    binding_sha256: String,
}

impl UninstallJournal {
    fn new(attempt_id: String, original: [FileStamp; 4]) -> Result<Self> {
        let mut journal = Self {
            schema_version: SCHEMA_VERSION,
            attempt_id,
            original,
            ancillary_removed: false,
            binding_sha256: String::new(),
        };
        journal.binding_sha256 = journal.binding()?;
        Ok(journal)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || !journal::valid_attempt_id(&self.attempt_id)
            || self.binding_sha256 != self.binding()?
        {
            bail!("managed-pair uninstall journal is invalid");
        }
        Ok(())
    }

    fn binding(&self) -> Result<String> {
        let mut clone = self.clone();
        clone.binding_sha256.clear();
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&clone)?)))
    }

    fn bytes(&mut self) -> Result<Vec<u8>> {
        self.binding_sha256 = self.binding()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_UNINSTALL_JOURNAL_BYTES {
            bail!("managed-pair uninstall journal exceeds its bound");
        }
        Ok(bytes)
    }
}

pub(super) fn prepare(layout: &Layout, verifier: &dyn ManagedPairVerifier) -> Result<String> {
    if journal::read(layout)?.is_some() {
        bail!("managed-pair upgrade transaction must be recovered before uninstall");
    }
    if super::attempt::read_begin(layout)?.is_some() {
        bail!("managed-pair begun upgrade must be aborted before uninstall");
    }
    if let Some(existing) = read(layout)? {
        require_original_or_absent(layout, &existing)?;
        return Ok(existing.attempt_id);
    }
    validate_active(layout, verifier)?;
    let original = Slot::ALL
        .into_iter()
        .map(|slot| {
            filesystem::stamp_optional(&layout.target(slot), max_bytes(slot), slot.label())?
                .ok_or_else(|| anyhow!("{} is absent before uninstall", slot.label()))
        })
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .map_err(|_| anyhow!("managed-pair uninstall slot count is invalid"))?;
    let mut journal = UninstallJournal::new(uuid::Uuid::new_v4().simple().to_string(), original)?;
    filesystem::write_new(
        &layout.uninstall_journal(),
        &journal.bytes()?,
        false,
        "managed-pair uninstall journal",
    )?;
    layout.revalidate()?;
    let reread = read(layout)?.ok_or_else(|| anyhow!("managed-pair uninstall journal vanished"))?;
    if reread.binding_sha256 != journal.binding_sha256 {
        bail!("managed-pair uninstall journal changed while being armed");
    }
    Ok(journal.attempt_id)
}

pub(super) fn execute(layout: &Layout, attempt_id: &str) -> Result<bool> {
    if journal::read(layout)?.is_some() {
        bail!("managed-pair upgrade transaction must be recovered before uninstall");
    }
    if super::attempt::read_begin(layout)?.is_some() {
        bail!("managed-pair begun upgrade must be aborted before uninstall");
    }
    let Some(mut journal) = read(layout)? else {
        let any_fixed_slot_remains = Slot::ALL.into_iter().try_fold(false, |present, slot| {
            Ok::<_, anyhow::Error>(
                present
                    || filesystem::stamp_optional(
                        &layout.target(slot),
                        max_bytes(slot),
                        slot.label(),
                    )?
                    .is_some(),
            )
        })?;
        if any_fixed_slot_remains {
            bail!("managed-pair uninstall journal is absent while fixed pair material remains");
        }
        return Ok(false);
    };
    if journal.attempt_id != attempt_id {
        bail!("managed-pair uninstall attempt does not match the armed transaction");
    }
    require_original_or_absent(layout, &journal)?;

    if !journal.ancillary_removed {
        // State is the acceptance commit marker, so it is always removed first.
        for slot in [Slot::State, Slot::Envelope, Slot::Companion] {
            filesystem::remove_if_exact(
                &layout.target(slot),
                &journal.original[slot.index()],
                max_bytes(slot),
                slot.label(),
            )?;
        }
        layout.remove_empty_candidate_base()?;
        journal.ancillary_removed = true;
        write(layout, &mut journal)?;
    }
    if let Some(actual) = filesystem::stamp_optional(
        &layout.target(Slot::Core),
        max_bytes(Slot::Core),
        Slot::Core.label(),
    )? {
        if actual != journal.original[Slot::Core.index()] {
            bail!("refusing substituted managed-pair Core during uninstall");
        }
        filesystem::remove_if_exact(
            &layout.target(Slot::Core),
            &actual,
            max_bytes(Slot::Core),
            Slot::Core.label(),
        )
        .map_err(|error| {
            anyhow!(
                "managed_pair_core_delete_retry_required: fixed Core remains handle-locked; retry after exit or reboot: {error:#}"
            )
        })?;
    }
    remove(layout, &journal)?;
    Ok(false)
}

pub(super) fn present(layout: &Layout) -> Result<bool> {
    read(layout).map(|journal| journal.is_some())
}

fn read(layout: &Layout) -> Result<Option<UninstallJournal>> {
    if let Some(temporary) = filesystem::read_temporary(
        &layout.uninstall_journal_temporary(),
        MAX_UNINSTALL_JOURNAL_BYTES,
        "managed-pair uninstall temporary",
    )? {
        filesystem::remove_temporary_exact(
            &layout.uninstall_journal_temporary(),
            &temporary.stamp,
            MAX_UNINSTALL_JOURNAL_BYTES,
            "managed-pair uninstall temporary",
        )?;
    }
    let observed = match filesystem::read_regular(
        &layout.uninstall_journal(),
        MAX_UNINSTALL_JOURNAL_BYTES,
        "managed-pair uninstall journal",
    ) {
        Ok(observed) => observed,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let journal: UninstallJournal =
        serde_json::from_slice(&observed.bytes).context("parse managed-pair uninstall journal")?;
    journal.validate()?;
    Ok(Some(journal))
}

fn write(layout: &Layout, journal: &mut UninstallJournal) -> Result<()> {
    let temporary = layout.uninstall_journal_temporary();
    filesystem::require_absent(&temporary, "managed-pair uninstall temporary")?;
    let stamp = filesystem::write_new(
        &temporary,
        &journal.bytes()?,
        false,
        "managed-pair uninstall temporary",
    )?;
    filesystem::durable_replace(
        &temporary,
        &layout.uninstall_journal(),
        &stamp,
        MAX_UNINSTALL_JOURNAL_BYTES,
        "managed-pair uninstall temporary",
    )
}

fn remove(layout: &Layout, journal: &UninstallJournal) -> Result<()> {
    let current =
        read(layout)?.ok_or_else(|| anyhow!("managed-pair uninstall journal disappeared"))?;
    if current.attempt_id != journal.attempt_id || current.binding_sha256 != journal.binding_sha256
    {
        bail!("refusing replaced managed-pair uninstall journal");
    }
    let stamp = filesystem::stamp_optional(
        &layout.uninstall_journal(),
        MAX_UNINSTALL_JOURNAL_BYTES,
        "managed-pair uninstall journal",
    )?
    .ok_or_else(|| anyhow!("managed-pair uninstall journal disappeared"))?;
    filesystem::remove_if_exact(
        &layout.uninstall_journal(),
        &stamp,
        MAX_UNINSTALL_JOURNAL_BYTES,
        "managed-pair uninstall journal",
    )
}

fn require_original_or_absent(layout: &Layout, journal: &UninstallJournal) -> Result<()> {
    layout.revalidate()?;
    for slot in Slot::ALL {
        if let Some(actual) =
            filesystem::stamp_optional(&layout.target(slot), max_bytes(slot), slot.label())?
        {
            if actual != journal.original[slot.index()] {
                bail!("refusing substituted {} during uninstall", slot.label());
            }
        }
    }
    Ok(())
}

const fn max_bytes(slot: Slot) -> u64 {
    match slot {
        Slot::Core | Slot::Companion => MAX_COMPONENT_BYTES,
        Slot::Envelope => MAX_ENVELOPE_BYTES,
        Slot::State => MAX_STATE_BYTES,
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
