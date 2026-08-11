use std::path::Path;

use anyhow::Result;
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_read_application::{
    CompactPresentationProjection, GenerationRead, GenerationReadRequest, RetainedPeerRead,
};
use ctx_history_refresh::verify_generation_query_authority;
use serde_json::Value;

pub(super) use ctx_history_read_application::reference_needs_retained_peer;

pub(super) fn generation_read(
    index: VerifiedIndex,
    index_root: &Path,
    request: &GenerationReadRequest,
) -> Result<GenerationRead> {
    let retained_peer = match request.retained_peer {
        RetainedPeerRead::Omit => None,
        RetainedPeerRead::IfAvailable => open_retained_peer(&index, index_root)?,
    };
    Ok(GenerationRead::new(index, retained_peer))
}

fn open_retained_peer(current: &VerifiedIndex, index_root: &Path) -> Result<Option<VerifiedIndex>> {
    let retained_peer = VerifiedIndex::open_retained_generation_peer(
        index_root,
        current.generation_id(),
    )
    .map_err(|error| match error {
        IndexError::PinnedGenerationNotRetained { .. } => IndexError::ConcurrentGenerationChange,
        error => error,
    })?;
    if let Some(peer) = retained_peer.as_ref() {
        verify_generation_query_authority(peer).map_err(anyhow::Error::new)?;
    }
    Ok(retained_peer)
}

/// CLI-owned generation opener around the application-owned compact projector.
pub(super) struct CompactPresentation<'index> {
    current: &'index VerifiedIndex,
    retained_peer: Option<VerifiedIndex>,
}

impl<'index> CompactPresentation<'index> {
    pub(super) fn open_if_needed(
        current: &'index VerifiedIndex,
        index_root: &Path,
        needed: bool,
    ) -> Result<Option<Self>> {
        needed.then(|| Self::open(current, index_root)).transpose()
    }

    pub(super) fn open(current: &'index VerifiedIndex, index_root: &Path) -> Result<Self> {
        let retained_peer = open_retained_peer(current, index_root)?;
        Ok(Self {
            current,
            retained_peer,
        })
    }

    pub(super) fn retained_peer(&self) -> Option<&VerifiedIndex> {
        self.retained_peer.as_ref()
    }

    pub(super) fn project(&self, value: &Value) -> Result<Value> {
        CompactPresentationProjection::new(self.current, self.retained_peer.as_ref()).project(value)
    }
}
