use super::*;
use crate::provider::source_backed::family::jsonl::{
    decode_bounded_checkpoint, encode_bounded_checkpoint,
};

pub(super) const MAX_PROJECTOR_CHECKPOINT_BYTES: usize = 40 * 1024;
const PROJECTOR_CHECKPOINT_VERSION: u32 = 3;
const PROJECTOR_CHECKPOINT_PREFIX: &str = "openclaw.projector-checkpoint.v3:";

pub(super) struct RestoredProjectorCheckpoint {
    pub(super) session: SessionCheckpoint,
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
}

fn checkpoint_value<R: crate::JsonlProviderRuntime>(
    projector: &OpenClawProjector<R>,
) -> ProjectorCheckpoint {
    ProjectorCheckpoint {
        version: PROJECTOR_CHECKPOINT_VERSION,
        native_session_id: projector.native_session_id.clone(),
        session: projector.session.checkpoint(),
    }
}

pub(super) fn encode_projector_checkpoint<R: crate::JsonlProviderRuntime>(
    projector: &OpenClawProjector<R>,
) -> Result<TypedKey> {
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
    let checkpoint: ProjectorCheckpoint =
        decode_bounded_checkpoint::<ProjectorCheckpoint, CaptureError>(
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
    Ok(RestoredProjectorCheckpoint {
        session: checkpoint.session,
    })
}
