//! Shared-family projection for Codex's ordinary `history.jsonl` prompt log.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CoreRecord, CoreRecordError, ProjectionContractError, SourceAnchor, SourceKey,
    TypedKey,
};
use thiserror::Error;

use super::super::absolute_lexical_path;
use super::PromptLine;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope, JsonlFamilyInventory,
            JsonlFamilyInventoryMode, JsonlFamilyLeaf, JsonlFamilyMembershipObservation,
            JsonlFamilyProjector, JsonlFamilyRootMissingMode, JsonlFamilyWorkerContext,
            JsonlOversizedRecordPolicy, JsonlRecordRef,
        },
        SourceBackedRouteErrorKind,
    },
    CaptureError,
};

mod projection;
use projection::{core_record, retained_record_bytes};

const SOURCE_FORMAT: &str = "codex_history_jsonl";
const SOURCE_SCHEMA_VARIANT: &str = "codex-prompt-history-jsonl-v1";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "codex-prompt-history-shared-jsonl-v4";
const SESSION_KEY_NAMESPACE: &str = "codex.prompt-history.session";
const EVENT_POSITION_KIND: &str = "codex.prompt-history.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "codex-prompt-history-session";
const LOGICAL_EVENT_KIND: &str = "codex-prompt-history-event";
const MAX_RETAINED_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum CodexPromptHistorySourceBackedErrorV0 {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Codex prompt-history Core record exceeds its retained-record bound")]
    RecordTooLarge,
}

pub(crate) type CodexPromptHistorySourceBackedResultV0<T> =
    Result<T, CodexPromptHistorySourceBackedErrorV0>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexPromptHistorySourceBackedInputV0 {
    path: PathBuf,
    catalog_lineage: [u8; 32],
}

impl CodexPromptHistorySourceBackedInputV0 {
    pub(crate) fn explicit(path: impl Into<PathBuf>, catalog_lineage: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            catalog_lineage,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source_key(&self) -> CodexPromptHistorySourceBackedResultV0<SourceKey> {
        Ok(SourceKey::derive(
            CaptureProvider::Codex.as_str(),
            SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(self.catalog_lineage),
        )?)
    }
}

#[cfg(test)]
#[derive(Default)]
struct CodexPromptHistoryJsonlFamilyStateV0 {
    after_scan_hook: Option<Box<dyn FnOnce() + Send>>,
    after_family_source_open_hook: Option<Box<dyn FnOnce() + Send>>,
}

/// The shared family owns framing, checkpoints, append classification, paging,
/// publication, deletion, and terminal validation. This adapter supplies only
/// Codex prompt discovery and per-record projection semantics.
#[derive(Clone)]
pub(crate) struct CodexPromptHistoryJsonlFamilyAdapterV0 {
    input: CodexPromptHistorySourceBackedInputV0,
    #[cfg(test)]
    state: Arc<Mutex<CodexPromptHistoryJsonlFamilyStateV0>>,
}

impl CodexPromptHistoryJsonlFamilyAdapterV0 {
    pub(crate) fn new(
        mut input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        let route_path = absolute_lexical_path(input.path())?;
        input.path = route_path.clone();
        Ok(Self {
            input,
            #[cfg(test)]
            state: Arc::new(Mutex::new(CodexPromptHistoryJsonlFamilyStateV0::default())),
        })
    }

    pub(crate) fn route_path(&self) -> &Path {
        self.input.path()
    }

    #[cfg(test)]
    fn set_after_scan_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("prompt-history state lock");
        assert!(state.after_scan_hook.is_none());
        state.after_scan_hook = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_after_family_source_open_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("prompt-history state lock");
        assert!(state.after_family_source_open_hook.is_none());
        state.after_family_source_open_hook = Some(Box::new(hook));
    }

    fn discover_family(&self, route_path: &Path) -> crate::Result<JsonlFamilyInventory> {
        if route_path != self.input.path() {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history JSONL route path changed".to_owned(),
            ));
        }
        let parent = route_path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Codex prompt-history JSONL path has no parent".to_owned())
        })?;
        let authority_path = route_path.file_name().map(PathBuf::from).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex prompt-history JSONL path has no filename".to_owned(),
            )
        })?;
        let retained = (|| -> crate::Result<_> {
            let authority = Arc::new(ProviderSourceRoot::open(parent)?);
            let opened = authority.open_file(&authority_path)?;
            Ok((authority, opened))
        })();
        let (authority, opened) = match retained {
            Ok(retained) => retained,
            Err(error) if capture_error_is_not_found(&error) => {
                return JsonlFamilyInventory::missing(CaptureProvider::Codex, route_path);
            }
            Err(error) => return Err(error),
        };
        #[cfg(test)]
        if let Some(hook) = self
            .state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .after_family_source_open_hook
            .take()
        {
            hook();
        }
        let source = self
            .input
            .source_key()
            .map_err(prompt_family_capture_error)?;
        let binding = TypedKey::bytes(source.exact_descriptor_digest().to_vec())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let leaf = JsonlFamilyLeaf::bind_opened(
            source,
            route_path.to_path_buf(),
            Arc::clone(&authority),
            authority_path,
            binding,
            &opened,
        )?;
        authority.revalidate()?;
        JsonlFamilyInventory::present(CaptureProvider::Codex, route_path, authority, vec![leaf])
    }
}

impl JsonlFamilyAdapter for CodexPromptHistoryJsonlFamilyAdapterV0 {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::AuthoritativeEmpty
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        self.discover_family(root)
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> crate::Result<JsonlFamilyMembershipObservation> {
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        prompt_family_error_kind(error, false)
    }

    fn scan_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        prompt_family_error_kind(error, true)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        let source = self
            .input
            .source_key()
            .map_err(prompt_family_capture_error)?;
        if !source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Box::new(CodexPromptHistoryProjector {
            source,
            rejected_records: 0,
        }))
    }

    fn finish_leaf_scans(&self) -> crate::Result<()> {
        #[cfg(test)]
        if let Some(hook) = self
            .state
            .lock()
            .map_err(|_| prompt_family_state_error())?
            .after_scan_hook
            .take()
        {
            hook();
        }
        Ok(())
    }

    fn base_source_path(
        &self,
        _certificate: &ctx_history_core::CertifiedSource,
    ) -> crate::Result<PathBuf> {
        Ok(self.input.path().to_path_buf())
    }
}

struct CodexPromptHistoryProjector {
    source: SourceKey,
    rejected_records: u64,
}

impl CodexPromptHistoryProjector {
    fn reject(&mut self) {
        self.rejected_records = self.rejected_records.saturating_add(1);
    }
}

impl JsonlFamilyProjector for CodexPromptHistoryProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
    ) -> crate::Result<()> {
        if record.oversized() {
            self.reject();
            return Ok(());
        }
        if record.bytes().iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let line = match serde_json::from_slice::<PromptLine>(record.bytes()) {
            Ok(line)
                if !line.session_id.trim().is_empty()
                    && chrono::DateTime::from_timestamp(line.ts, 0).is_some() =>
            {
                line
            }
            _ => {
                self.reject();
                return Ok(());
            }
        };
        let projected = core_record(&self.source, line, record.evidence().physical_ordinal())
            .map_err(prompt_family_capture_error)?;
        if retained_record_bytes(&projected) > MAX_RETAINED_RECORD_BYTES {
            return Err(CaptureError::InvalidPayload(
                CodexPromptHistorySourceBackedErrorV0::RecordTooLarge.to_string(),
            ));
        }
        emit(projected)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

fn capture_error_is_not_found(error: &CaptureError) -> bool {
    match error {
        CaptureError::Io(error) => error.kind() == std::io::ErrorKind::NotFound,
        CaptureError::SystemIo { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

fn prompt_family_capture_error(error: CodexPromptHistorySourceBackedErrorV0) -> CaptureError {
    match error {
        CodexPromptHistorySourceBackedErrorV0::Capture(error) => error,
        CodexPromptHistorySourceBackedErrorV0::Json(error) => CaptureError::Json(error),
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

#[cfg(test)]
fn prompt_family_state_error() -> CaptureError {
    CaptureError::InvalidPayload(
        "Codex prompt-history JSONL family state lock was poisoned".to_owned(),
    )
}

fn prompt_family_error_kind(error: &CaptureError, scanning: bool) -> SourceBackedRouteErrorKind {
    match error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound && scanning => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    }
}

#[cfg(test)]
mod tests;
