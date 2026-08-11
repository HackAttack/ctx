use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_index::{BaseEventIdentityLookup, IndexError};
use uuid::Uuid;

/// Capture-local adapter for the index-owned immutable base identity view.
///
/// This is deliberately a transparent compile-time boundary: capture callers
/// keep the concrete type, while the index remains the sole lookup authority.
#[repr(transparent)]
#[derive(Clone)]
pub(crate) struct IndexBaseEventLookup(BaseEventIdentityLookup);

impl From<BaseEventIdentityLookup> for IndexBaseEventLookup {
    fn from(lookup: BaseEventIdentityLookup) -> Self {
        Self(lookup)
    }
}

impl BaseEventLookup for IndexBaseEventLookup {
    type Error = IndexError;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
        self.0.contains(event_id)
    }
}
