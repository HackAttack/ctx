use anyhow::{anyhow, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_index_query::{CoreEventRecord, EventRecord, SessionRecord};

use crate::{resolve_core_event_with_refs, resolve_show_session_with_refs, PinnedHistoryQuery};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateRequest {
    Event {
        selector: String,
    },
    Session {
        selector: Option<String>,
        provider_session_id: Option<String>,
        provider: Option<CaptureProvider>,
    },
}

#[derive(Debug)]
pub enum LocateResult {
    Event(CoreEventRecord),
    Session {
        session: SessionRecord,
        first_event: EventRecord,
    },
}

impl PinnedHistoryQuery<'_> {
    pub fn locate(&self, request: &LocateRequest) -> Result<LocateResult> {
        match request {
            LocateRequest::Event { selector } => {
                resolve_core_event_with_refs(&self.references, selector).map(LocateResult::Event)
            }
            LocateRequest::Session {
                selector,
                provider_session_id,
                provider,
            } => {
                let session = resolve_show_session_with_refs(
                    &self.references,
                    selector.as_deref(),
                    provider_session_id.as_deref(),
                    *provider,
                )?;
                let first_event = self
                    .index
                    .events_for_session(session.session_id.as_uuid())?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        anyhow!(
                            "session {} has no event in the pinned Core generation",
                            session.session_id
                        )
                    })?;
                Ok(LocateResult::Session {
                    session,
                    first_event,
                })
            }
        }
    }
}
