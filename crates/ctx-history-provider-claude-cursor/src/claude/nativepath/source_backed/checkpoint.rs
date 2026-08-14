use super::*;
use ctx_history_provider_runtime::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
    restore_hash_pending_exchange_entries, sorted_pending_exchange_entries,
};

pub(super) const MAX_PROJECTOR_CHECKPOINT_BYTES: usize = 40 * 1024;
const PROJECTOR_CHECKPOINT_VERSION: u32 = 2;
const PROJECTOR_CHECKPOINT_PREFIX: &str = "claude.projector-checkpoint.v2:";

pub(super) struct RestoredProjectorCheckpoint {
    pub(super) session: ClaudeSessionMetadata,
    pub(super) pending_calls: HashMap<String, PendingCallState>,
    pub(super) linkage_capacity_exceeded: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectorCheckpoint {
    version: u32,
    session: ClaudeSessionMetadata,
    pending_calls: Vec<(String, PendingCallState)>,
    linkage_capacity_exceeded: bool,
}

fn checkpoint_value<B: ProviderRuntimeBinding>(
    projector: &ClaudeProjector<B>,
) -> ProjectorCheckpoint {
    ProjectorCheckpoint {
        version: PROJECTOR_CHECKPOINT_VERSION,
        session: projector.session.clone(),
        pending_calls: sorted_pending_exchange_entries(&projector.pending_calls),
        linkage_capacity_exceeded: projector.linkage_capacity_exceeded,
    }
}

pub(super) fn projector_checkpoint_fits<B: ProviderRuntimeBinding>(
    projector: &ClaudeProjector<B>,
) -> bool {
    bounded_checkpoint_fits(
        &checkpoint_value::<B>(projector),
        MAX_PROJECTOR_CHECKPOINT_BYTES,
    )
}

pub(super) fn encode_projector_checkpoint<B: ProviderRuntimeBinding>(
    projector: &ClaudeProjector<B>,
) -> Result<TypedKey> {
    encode_bounded_checkpoint(
        PROJECTOR_CHECKPOINT_PREFIX,
        &checkpoint_value::<B>(projector),
        MAX_PROJECTOR_CHECKPOINT_BYTES,
        "Claude",
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
        "Claude",
    )?;
    if checkpoint.version != PROJECTOR_CHECKPOINT_VERSION || checkpoint.session.key != binding.key {
        return Err(CaptureError::InvalidPayload(
            "Claude projector checkpoint does not match its source binding".to_owned(),
        ));
    }
    if checkpoint.pending_calls.len() > MAX_PENDING_CALLS {
        return Err(CaptureError::InvalidPayload(
            "Claude projector checkpoint exceeds its state capacity".to_owned(),
        ));
    }
    let pending_calls = restore_hash_pending_exchange_entries(checkpoint.pending_calls)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Claude projector checkpoint repeats a call identity".to_owned(),
            )
        })?;
    Ok(RestoredProjectorCheckpoint {
        session: checkpoint.session,
        pending_calls,
        linkage_capacity_exceeded: checkpoint.linkage_capacity_exceeded,
    })
}
