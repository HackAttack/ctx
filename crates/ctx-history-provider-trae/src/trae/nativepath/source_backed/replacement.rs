use super::*;
use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::{
    CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
    ReplacementDocumentTree, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};
use ctx_history_provider_runtime::{
    invalid_route_error, ProviderChangedDocumentSink, ProviderRouteControlExpectation,
    ProviderRuntimeBinding,
};
use ctx_history_source_sqlite::SqliteSourceAccessError;
use std::marker::PhantomData;

#[derive(Debug, Clone)]
pub struct TraeReplacementTree<B: ProviderRuntimeBinding> {
    data_root: PathBuf,
    path: PathBuf,
    binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> TraeReplacementTree<B> {
    pub fn new(data_root: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            path: path.into(),
            binding: PhantomData,
        }
    }
}

pub struct TraeTreeAuthority {
    canonical_path: PathBuf,
    source: TraeSourceAuthority,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

impl<B: ProviderRuntimeBinding> ReplacementDocumentTree for TraeReplacementTree<B> {
    type Lifecycle = B::CaptureLifecycleSink;
    type Spool = B::DocumentRecordSpool;
    type RouteControl = ProviderRouteControlExpectation;
    type Leaf = SourceKey;
    type TreeAuthority = TraeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        TRAE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Trae.as_str()
            && source.source_format() == TRAE_STATE_VSCDB_SOURCE_FORMAT
            && source.schema_variant() == TRAE_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let canonical_path = explicit_trae_leaf(&self.path).map_err(|error| {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
        })?;
        let authority = acquire_source(
            &self.data_root,
            &canonical_path,
            DateTime::<Utc>::UNIX_EPOCH,
        )
        .map_err(route_error)?;
        let source = source_key(&authority).map_err(route_error)?;
        let fingerprint = DocumentLeafFingerprint::new(authority.logical_fingerprint);
        let terminal_revalidate = authority
            .database
            .terminal_revalidator()
            .map_err(route_error)?;
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::new(fingerprint, source)],
            TraeTreeAuthority {
                canonical_path,
                source: authority,
                terminal_revalidate,
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        source: &Self::Leaf,
        sink: &mut ProviderChangedDocumentSink<'_, '_, B>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        sink.begin_source(source.clone())?;
        let mut sink_error = None;
        let scan = scan_trae_authority(&authority.canonical_path, &authority.source, &mut |page| {
            for document in page.documents {
                if let Err(error) = sink.emit_core_record(document) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(TraeSourceBackedErrorV0::Capture(
                        CaptureError::InvalidPayload(detail),
                    ));
                }
            }
            Ok(())
        });
        if let Some(error) = sink_error {
            return Err(error);
        }
        let scan = scan.map_err(route_error)?;
        let counts = scan.source.counts();
        if scan.row_decode_passes != 1
            || scan.decoded_rows > counts.complete_records
            || scan.peak_buffered_documents > TRAE_SOURCE_BACKED_PAGE_MAX_UNITS as u64
            || (counts.indexed_documents == 0) != (scan.emitted_pages == 0)
        {
            return Err(trae_internal(
                "Trae scan violated its one-pass bounded-page receipt",
            ));
        }
        if !scan
            .source
            .observation()
            .source()
            .exact_descriptor_eq(source)
            || scan.terminal_fence.evidence != *authority.source.database.evidence()
        {
            return Err(trae_changed(
                "Trae source changed between physical discovery and logical scan",
            ));
        }
        Ok(document_terminal(scan.source))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .source
            .database
            .seal_if_active(&tree.authority.canonical_path)
            .map_err(route_error)?;
        tree.authority
            .source
            .workspace_folder
            .revalidate()
            .map_err(route_error)?;
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

fn document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: TRAE_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn trae_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn trae_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    invalid_route_error(error)
}
