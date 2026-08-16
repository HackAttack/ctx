use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{filesystem, journal, Layout};

const SCHEMA_VERSION: u32 = 1;
const MAX_CONTROL_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TerminalOutcome {
    Committed,
    Aborted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TerminalReceipt {
    schema_version: u32,
    pub(super) attempt_id: String,
    pub(super) outcome: TerminalOutcome,
    pub(super) error_code: Option<String>,
    binding_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BeginRecord {
    schema_version: u32,
    attempt_id: String,
    binding_sha256: String,
}

impl BeginRecord {
    fn binding(&self) -> Result<String> {
        let mut clone = self.clone();
        clone.binding_sha256.clear();
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&clone)?)))
    }
}

impl TerminalReceipt {
    fn binding(&self) -> Result<String> {
        let mut clone = self.clone();
        clone.binding_sha256.clear();
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&clone)?)))
    }
}

pub(super) fn read_begin(layout: &Layout) -> Result<Option<String>> {
    let Some(bytes) = read_optional(&layout.begin_record(), "managed-pair begin record")? else {
        return Ok(None);
    };
    let record: BeginRecord =
        serde_json::from_slice(&bytes).context("parse managed-pair begin record")?;
    if record.schema_version != SCHEMA_VERSION
        || !journal::valid_attempt_id(&record.attempt_id)
        || record.binding_sha256 != record.binding()?
    {
        bail!("managed-pair begin record is invalid");
    }
    Ok(Some(record.attempt_id))
}

pub(super) fn write_begin(layout: &Layout, attempt_id: &str) -> Result<()> {
    if !journal::valid_attempt_id(attempt_id) {
        bail!("managed-pair begin attempt is invalid");
    }
    let mut record = BeginRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: attempt_id.to_owned(),
        binding_sha256: String::new(),
    };
    record.binding_sha256 = record.binding()?;
    filesystem::write_new(
        &layout.begin_record(),
        &encoded(&record)?,
        false,
        "managed-pair begin record",
    )?;
    Ok(())
}

pub(super) fn remove_begin(layout: &Layout, attempt_id: &str) -> Result<()> {
    let Some(current) = read_begin(layout)? else {
        return Ok(());
    };
    if current != attempt_id {
        bail!("refusing to remove a different managed-pair begin record");
    }
    remove_optional(&layout.begin_record(), "managed-pair begin record")
}

pub(super) fn read_terminal(layout: &Layout) -> Result<Option<TerminalReceipt>> {
    cleanup_temporary(layout)?;
    let Some(bytes) = read_optional(&layout.terminal_receipt(), "managed-pair terminal receipt")?
    else {
        return Ok(None);
    };
    let receipt: TerminalReceipt =
        serde_json::from_slice(&bytes).context("parse managed-pair terminal receipt")?;
    if receipt.schema_version != SCHEMA_VERSION
        || !journal::valid_attempt_id(&receipt.attempt_id)
        || receipt
            .error_code
            .as_ref()
            .is_some_and(|value| !valid_error_code(value))
        || receipt.binding_sha256 != receipt.binding()?
    {
        bail!("managed-pair terminal receipt is invalid");
    }
    Ok(Some(receipt))
}

pub(super) fn write_terminal(
    layout: &Layout,
    attempt_id: &str,
    outcome: TerminalOutcome,
    error_code: Option<&str>,
) -> Result<()> {
    if !journal::valid_attempt_id(attempt_id)
        || error_code.is_some_and(|value| !valid_error_code(value))
    {
        bail!("managed-pair terminal receipt identity is invalid");
    }
    cleanup_temporary(layout)?;
    let mut receipt = TerminalReceipt {
        schema_version: SCHEMA_VERSION,
        attempt_id: attempt_id.to_owned(),
        outcome,
        error_code: error_code.map(str::to_owned),
        binding_sha256: String::new(),
    };
    receipt.binding_sha256 = receipt.binding()?;
    let bytes = encoded(&receipt)?;
    let temporary = layout.terminal_receipt_temporary();
    let stamp = filesystem::write_new(
        &temporary,
        &bytes,
        false,
        "managed-pair terminal receipt temporary",
    )?;
    filesystem::durable_replace(
        &temporary,
        &layout.terminal_receipt(),
        &stamp,
        MAX_CONTROL_BYTES,
        "managed-pair terminal receipt temporary",
    )
}

fn cleanup_temporary(layout: &Layout) -> Result<()> {
    let temporary = layout.terminal_receipt_temporary();
    if let Some(observed) = filesystem::read_temporary(
        &temporary,
        MAX_CONTROL_BYTES,
        "managed-pair terminal receipt temporary",
    )? {
        filesystem::remove_temporary_exact(
            &temporary,
            &observed.stamp,
            MAX_CONTROL_BYTES,
            "managed-pair terminal receipt temporary",
        )?;
    }
    Ok(())
}

fn read_optional(entry: &filesystem::Entry, label: &str) -> Result<Option<Vec<u8>>> {
    match filesystem::read_regular(entry, MAX_CONTROL_BYTES, label) {
        Ok(observed) => Ok(Some(observed.bytes)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_optional(entry: &filesystem::Entry, label: &str) -> Result<()> {
    let Some(stamp) = filesystem::stamp_optional(entry, MAX_CONTROL_BYTES, label)? else {
        return Ok(());
    };
    filesystem::remove_if_exact(entry, &stamp, MAX_CONTROL_BYTES, label)
}

fn encoded(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        bail!("managed-pair control record exceeds its bound");
    }
    Ok(bytes)
}

fn valid_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
