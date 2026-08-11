use std::sync::Mutex;

use super::*;
use crate::provider::source_backed::{
    family::document::{
        ChangedDocumentSink, CompleteDocumentTree, DocumentSourceTerminal, ReplacementDocumentTree,
    },
    route_error as default_route_error, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};

pub(crate) struct HermesTreeAuthority {
    opening_evidence: SqliteSourceEvidence,
    schema: HermesSchema,
    _schema_evidence: Vec<u8>,
    _sqlite_authority: SqliteSourceDirectoryAuthority,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

impl ReplacementDocumentTree for HermesSourceCandidate {
    type Leaf = HermesSessionLeaf;
    type TreeAuthority = HermesTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        HERMES_SOURCE_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Hermes.as_str()
            && source.source_format() == HERMES_SQLITE_SOURCE_FORMAT
            && matches!(
                source.schema_variant(),
                HERMES_SESSION_SOURCE_SCHEMA_VARIANT | HERMES_CONTROL_SOURCE_SCHEMA_VARIANT
            )
            && source.provider_identity_version() == 1
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(leaf.source.clone()))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_progress(&[], &mut |_| Ok(()))
    }

    fn discover_complete_with_progress(
        &self,
        base_sources: &[CertifiedSource],
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_reconciliation(
            base_sources,
            SourceBackedReconciliationDemand::Exhaustive,
            report_progress,
        )
    }

    fn discover_complete_with_reconciliation(
        &self,
        base_sources: &[CertifiedSource],
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        if std::fs::symlink_metadata(self.path()).is_err() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "selected Hermes database is unavailable",
            ));
        }
        let (sqlite_authority, snapshot) = open_root_authorized_snapshot_with_progress(
            &self.data_root,
            self.path(),
            report_progress,
        )
        .map_err(hermes_route_error)?;
        let opening_evidence = snapshot.evidence().clone();
        if let Err(error) = snapshot.revalidate().map_err(hermes_sqlite_route_error) {
            return Err(abort_hermes_route_snapshot(snapshot, error));
        }
        let inventory = match observe_hermes_reconciliation_inventory(
            self,
            snapshot.connection().map_err(hermes_sqlite_route_error)?,
            base_sources,
            reconciliation_demand,
            *opening_evidence.identity(),
            hermes_now_ms(),
            report_progress,
        ) {
            Ok(inventory) => inventory,
            Err(error) => {
                return Err(abort_hermes_route_snapshot(
                    snapshot,
                    hermes_route_error(error),
                ))
            }
        };
        Ok(CompleteDocumentTree::new(
            inventory.tree_fingerprint,
            inventory.leaves,
            HermesTreeAuthority {
                opening_evidence,
                schema: inventory.schema,
                _schema_evidence: inventory.schema_evidence,
                _sqlite_authority: sqlite_authority,
                terminal_revalidate: snapshot.terminal_revalidator(),
                snapshot: Mutex::new(Some(snapshot)),
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        if let Some(receipt) = &leaf.control_receipt {
            let expected = hermes_control_source_key(&self.source).map_err(hermes_route_error)?;
            let encoded = serde_json::to_vec(receipt)
                .map_err(HermesSourceBackedError::from)
                .map_err(hermes_route_error)?;
            if !expected.exact_descriptor_eq(&leaf.source) || encoded != leaf.observation_revision {
                return Err(hermes_changed(
                    "Hermes refresh control source changed after inventory observation",
                ));
            }
            sink.begin_source(leaf.source.clone())?;
            let mut digest = Sha256::new();
            digest.update(HERMES_CONTROL_FINGERPRINT_DOMAIN);
            digest.update(&leaf.observation_revision);
            let observation = SourceObservation::new(
                leaf.source.clone(),
                HERMES_CONTROL_OBSERVATION_KIND,
                leaf.observation_revision.clone(),
            )
            .map_err(HermesSourceBackedError::from)
            .map_err(hermes_route_error)?;
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                HERMES_SOURCE_PARSER_REVISION,
                digest.finalize().into(),
                ScannedSourceCounts::default(),
            )
            .map_err(HermesSourceBackedError::from)
            .map_err(hermes_route_error)?;
            return Ok(document_terminal(certificate));
        }
        let expected = hermes_session_source_key(&self.source, &leaf.provider_session_id)
            .map_err(hermes_route_error)?;
        if !expected.exact_descriptor_eq(&leaf.source) {
            return Err(hermes_changed(
                "Hermes logical session source changed after inventory observation",
            ));
        }
        let snapshot = take_snapshot(&authority.snapshot)?;
        let scan = (|| {
            sink.begin_source(leaf.source.clone())?;
            let mut sink_error = None;
            let scan = project_hermes_session_snapshot_with_progress(
                self,
                leaf,
                &authority.schema,
                snapshot.connection().map_err(hermes_sqlite_route_error)?,
                &mut |output| match output {
                    HermesSnapshotProjectionOutput::Page(page) => {
                        if let Err(error) = sink.report_completed_bytes(page.completed_bytes) {
                            let detail = error.to_string();
                            sink_error = Some(error);
                            return Err(HermesSourceBackedError::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                        for record in page.records {
                            if let HermesSourceBackedRecord::Event(document) = record {
                                if let Err(error) = sink.emit_core_record(document) {
                                    let detail = error.to_string();
                                    sink_error = Some(error);
                                    return Err(HermesSourceBackedError::Capture(
                                        CaptureError::InvalidPayload(detail),
                                    ));
                                }
                            }
                        }
                        Ok(())
                    }
                    HermesSnapshotProjectionOutput::Progress(progress) => sink
                        .report_current_source_progress(progress)
                        .map_err(|error| {
                            sink_error = Some(error.clone());
                            HermesSourceBackedError::Route(error)
                        }),
                },
            );
            if let Some(error) = sink_error {
                return Err(error);
            }
            let scan = scan.map_err(hermes_route_error)?;
            let counts = scan.certificate.counts();
            if scan.decoded_rows != counts.complete_records
                || scan.peak_buffered_records > 64
                || (counts.complete_records == 0) != (scan.emitted_pages == 0)
                || scan.native_candidate_query_batches == 0
                || scan.native_hydration_query_batches > scan.native_candidate_query_batches
                || scan.max_native_rows_per_set > 64
            {
                return Err(hermes_internal(
                    "Hermes scan violated its one-pass bounded-page receipt",
                ));
            }
            if snapshot.evidence() != &authority.opening_evidence {
                return Err(hermes_changed(
                    "Hermes source changed between physical discovery and logical scan",
                ));
            }
            snapshot.revalidate().map_err(hermes_sqlite_route_error)?;
            Ok(scan)
        })();
        let scan = match scan {
            Ok(scan) => scan,
            Err(error) => return Err(abort_hermes_route_snapshot(snapshot, error)),
        };
        if let Err(failure) = restore_snapshot(&authority.snapshot, snapshot) {
            let (error, snapshot) = *failure;
            return Err(abort_hermes_route_snapshot(snapshot, error));
        }
        Ok(document_terminal(scan.certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let snapshot = take_snapshot(&tree.authority.snapshot)?;
        let evidence = route_hermes_terminal_revalidation(snapshot.finish())?;
        if evidence != tree.authority.opening_evidence {
            return Err(hermes_changed(format!(
                "{}: physical source changed before commit",
                HermesSourceBackedError::SourceChanged
            )));
        }
        route_hermes_terminal_revalidation((tree.authority.terminal_revalidate)())?;
        Ok(tree.tree_fingerprint)
    }
}

pub(super) fn hermes_route_error(error: HermesSourceBackedError) -> SourceBackedRouteError {
    let error = match error {
        HermesSourceBackedError::Route(error) => return error,
        error => error,
    };
    let kind = match &error {
        HermesSourceBackedError::SqliteSource(error) if error.is_source_changed() => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        HermesSourceBackedError::SqliteSource(error) if error.is_systemic_resource_failure() => {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::SqliteSource(error) if error.is_ctx_owned_corruption() => {
            SourceBackedRouteErrorKind::Internal
        }
        HermesSourceBackedError::Capture(CaptureError::Io(error))
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::Capture(CaptureError::SystemIo { source, .. })
            if crate::provider_sources::resource_exhaustion_io_error(source) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::Capture(CaptureError::Sqlite(error))
            if crate::provider_sources::rusqlite_resource_failure(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        _ => return default_route_error(error),
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

pub(super) fn hermes_sqlite_route_error(error: SqliteSourceAccessError) -> SourceBackedRouteError {
    hermes_route_error(error.into())
}

pub(super) fn route_hermes_terminal_revalidation<T>(
    result: Result<T, SqliteSourceAccessError>,
) -> SourceBackedRouteResult<T> {
    result.map_err(hermes_sqlite_route_error)
}

fn abort_hermes_route_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: SourceBackedRouteError,
) -> SourceBackedRouteError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
            primary,
            hermes_sqlite_route_error(cleanup),
        ),
    }
}

fn take_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    slot.lock()
        .map_err(|_| hermes_internal("Hermes SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| hermes_internal("Hermes SQLite snapshot was already consumed"))
}

fn restore_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    snapshot: SqliteSourceReadSnapshot,
) -> Result<(), Box<(SourceBackedRouteError, SqliteSourceReadSnapshot)>> {
    let mut slot = match slot.lock() {
        Ok(slot) => slot,
        Err(_) => {
            return Err(Box::new((
                hermes_internal("Hermes SQLite snapshot lock was poisoned"),
                snapshot,
            )));
        }
    };
    if slot.is_some() {
        return Err(Box::new((
            hermes_internal("Hermes SQLite snapshot slot was already occupied"),
            snapshot,
        )));
    }
    *slot = Some(snapshot);
    Ok(())
}

fn document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: HERMES_SOURCE_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn hermes_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn hermes_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::provider_sources::SqliteRetryDecision;
    use rusqlite::ffi;

    #[test]
    fn real_hermes_projection_full_failure_is_systemic() {
        let directory = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("full.sqlite")).unwrap();
        connection
            .execute_batch(
                "PRAGMA page_size=512;
                 PRAGMA max_page_count=2;
                 CREATE TABLE payload(value BLOB)",
            )
            .unwrap();
        let sqlite = (0..128)
            .find_map(|_| {
                connection
                    .execute("INSERT INTO payload VALUES (zeroblob(4096))", [])
                    .err()
            })
            .unwrap();
        let diagnosed = diagnose_hermes_query_error(
            HermesSourceBackedError::Capture(CaptureError::Sqlite(sqlite)),
            SqliteFailurePhase::Projection,
        );
        let HermesSourceBackedError::SqliteSource(source) = &diagnosed else {
            panic!("unexpected Hermes error: {diagnosed:?}");
        };
        let diagnostic = source.diagnostic().unwrap();
        assert_eq!(diagnostic.phase, SqliteFailurePhase::Projection);
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
        assert_eq!(diagnostic.sqlite_primary_code, Some(ffi::SQLITE_FULL));
        assert_eq!(
            crate::provider_sources::sqlite_retry_decision(source),
            SqliteRetryDecision::RouteFatalResource
        );
        assert_eq!(
            hermes_route_error(diagnosed).kind,
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
    }
}
