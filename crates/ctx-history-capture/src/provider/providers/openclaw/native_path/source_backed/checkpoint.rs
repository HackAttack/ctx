use super::*;
use crate::provider::source_backed::family::jsonl::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
    restore_hash_pending_exchange_entries, sorted_pending_exchange_entries,
};

pub(super) const MAX_PROJECTOR_CHECKPOINT_BYTES: usize = 40 * 1024;
const PROJECTOR_CHECKPOINT_VERSION: u32 = 2;
const PROJECTOR_CHECKPOINT_PREFIX: &str = "openclaw.projector-checkpoint.v2:";

pub(super) struct RestoredProjectorCheckpoint {
    pub(super) session: SessionCheckpoint,
    pub(super) pending_calls: HashMap<String, PendingCallState>,
    pub(super) running_processes: HashMap<String, PendingCallState>,
    pub(super) linkage_capacity_exceeded: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionCheckpoint {
    pub(super) provider_session_id: String,
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) branch: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectorCheckpoint {
    version: u32,
    native_session_id: String,
    session: SessionCheckpoint,
    pending_calls: Vec<(String, PendingCallState)>,
    running_processes: Vec<(String, PendingCallState)>,
    linkage_capacity_exceeded: bool,
}

fn checkpoint_value(projector: &OpenClawProjector) -> ProjectorCheckpoint {
    ProjectorCheckpoint {
        version: PROJECTOR_CHECKPOINT_VERSION,
        native_session_id: projector.native_session_id.clone(),
        session: projector.session.checkpoint(),
        pending_calls: sorted_pending_exchange_entries(&projector.pending_calls),
        running_processes: sorted_pending_exchange_entries(&projector.running_processes),
        linkage_capacity_exceeded: projector.linkage_capacity_exceeded,
    }
}

pub(super) fn projector_checkpoint_fits(projector: &OpenClawProjector) -> bool {
    bounded_checkpoint_fits(&checkpoint_value(projector), MAX_PROJECTOR_CHECKPOINT_BYTES)
}

pub(super) fn encode_projector_checkpoint(projector: &OpenClawProjector) -> Result<TypedKey> {
    encode_bounded_checkpoint(
        PROJECTOR_CHECKPOINT_PREFIX,
        &checkpoint_value(projector),
        MAX_PROJECTOR_CHECKPOINT_BYTES,
        "OpenClaw",
    )
}

pub(super) fn decode_projector_checkpoint(
    checkpoint: &TypedKey,
    binding: &Binding,
) -> Result<RestoredProjectorCheckpoint> {
    let checkpoint: ProjectorCheckpoint = decode_bounded_checkpoint(
        checkpoint,
        PROJECTOR_CHECKPOINT_PREFIX,
        MAX_PROJECTOR_CHECKPOINT_BYTES,
        "OpenClaw",
    )?;
    if checkpoint.version != PROJECTOR_CHECKPOINT_VERSION
        || checkpoint.native_session_id != binding.native_session_id
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw projector checkpoint does not match its source binding".to_owned(),
        ));
    }
    if checkpoint.pending_calls.len() > MAX_PENDING_CALLS
        || checkpoint.running_processes.len() > MAX_RUNNING_PROCESSES
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw projector checkpoint exceeds its state capacity".to_owned(),
        ));
    }
    Ok(RestoredProjectorCheckpoint {
        session: checkpoint.session,
        pending_calls: restore_pending_states(checkpoint.pending_calls, "call")?,
        running_processes: restore_pending_states(checkpoint.running_processes, "process session")?,
        linkage_capacity_exceeded: checkpoint.linkage_capacity_exceeded,
    })
}

fn restore_pending_states(
    entries: Vec<(String, PendingCallState)>,
    identity_kind: &str,
) -> Result<HashMap<String, PendingCallState>> {
    restore_hash_pending_exchange_entries(entries).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "OpenClaw projector checkpoint repeats a {identity_kind} identity"
        ))
    })
}
