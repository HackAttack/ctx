//! Shared-family projection for Codex's ordinary `history.jsonl` prompt log.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CoreRecord, CoreRecordError, ProjectionContractError, SourceAnchor, SourceKey,
};
use thiserror::Error;

use super::super::absolute_lexical_path;
use super::PromptLine;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::{
        family::jsonl::{
            jsonl_single_file_inventory, JsonlFamilyAdapter, JsonlFamilyAppendMode,
            JsonlFamilyBaseScope, JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
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
const PATH_KIND: &str = "Codex prompt-history JSONL";
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
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
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

/// The shared family owns framing, checkpoints, append classification, paging,
/// publication, deletion, and terminal validation. This adapter supplies only
/// the exact source binding and per-record Codex projection.
#[derive(Clone)]
pub(crate) struct CodexPromptHistoryJsonlFamilyAdapterV0 {
    route_path: Box<Path>,
    source: SourceKey,
}

impl CodexPromptHistoryJsonlFamilyAdapterV0 {
    pub(crate) fn new(
        input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        Ok(Self {
            route_path: absolute_lexical_path(&input.path)?.into_boxed_path(),
            source: input.source_key()?,
        })
    }

    pub(crate) fn route_path(&self) -> &Path {
        &self.route_path
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
        if root != self.route_path() {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history JSONL route path changed".to_owned(),
            ));
        }
        jsonl_single_file_inventory(self.provider(), root, self.source.clone(), PATH_KIND)
    }

    fn discovery_error_kind(&self, error: &CaptureError) -> SourceBackedRouteErrorKind {
        if matches!(error, CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            SourceBackedRouteErrorKind::Unavailable
        } else {
            SourceBackedRouteErrorKind::InvalidSource
        }
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        if !self.source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Box::new(CodexPromptHistoryProjector {
            source: self.source.clone(),
            rejected_records: 0,
        }))
    }

    fn base_source_path(
        &self,
        _certificate: &ctx_history_core::CertifiedSource,
    ) -> crate::Result<PathBuf> {
        Ok(self.route_path().to_owned())
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
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
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

#[cfg(test)]
mod tests;
