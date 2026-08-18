use super::*;
use ctx_history_capture_runtime::SourceBackedSourceOutcome;

#[derive(Debug)]
pub(crate) struct TerminalSourceEvidence<E: JsonlFamilyError> {
    pub(crate) certificate: CertifiedSource,
    pub(crate) terminal_certificate: Option<CertifiedSource>,
    pub(crate) terminal_proof: JsonlFamilyTerminalProof<E>,
    pub(crate) emitted_bytes: u64,
    pub(crate) exact_scan_bytes: Option<u64>,
    pub(crate) record_rejections: SourceBackedRecordRejectionDrafts,
}

impl<E: JsonlFamilyError> Clone for TerminalSourceEvidence<E> {
    fn clone(&self) -> Self {
        Self {
            certificate: self.certificate.clone(),
            terminal_certificate: self.terminal_certificate.clone(),
            terminal_proof: self.terminal_proof.clone(),
            emitted_bytes: self.emitted_bytes,
            exact_scan_bytes: self.exact_scan_bytes,
            record_rejections: self.record_rejections.clone(),
        }
    }
}

impl<E: JsonlFamilyError> TerminalSourceEvidence<E> {
    pub(crate) fn observed_certificate(&self) -> &CertifiedSource {
        self.terminal_certificate
            .as_ref()
            .unwrap_or(&self.certificate)
    }
}

pub(super) fn candidate_would_replace_retained_records_with_only_rejections(
    certificate: &CertifiedSource,
    base: Option<&CertifiedSource>,
) -> bool {
    let counts = certificate.counts();
    let retained_base_records = base
        .map(CertifiedSource::counts)
        .map_or(0, |counts| counts.retained_records);
    retained_base_records > 0
        && counts.complete_records > 0
        && counts.retained_records == 0
        && counts.rejected_records > 0
}

pub(super) fn reconcile_parallel_source_outcomes<E: JsonlFamilyError>(
    outcomes: Vec<SourceBackedSourceOutcome<TerminalSourceEvidence<E>>>,
    failed_evidences: Vec<TerminalSourceEvidence<E>>,
) -> SourceBackedRouteResult<Vec<TerminalSourceEvidence<E>>> {
    let mut failed_evidences = failed_evidences
        .into_iter()
        .map(|evidence| {
            (
                evidence
                    .observed_certificate()
                    .observation()
                    .source()
                    .exact_descriptor_digest(),
                evidence,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut evidences = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            SourceBackedSourceOutcome::Success(evidence) => evidences.push(evidence),
            SourceBackedSourceOutcome::Failed(mut failure) => {
                if !failure.failure.kind.is_logical_source_failure() {
                    return Err(route_internal(
                        "parallel JSONL source failure was not logical",
                    ));
                }
                let mut evidence = failed_evidences
                    .remove(&failure.source.exact_descriptor_digest())
                    .ok_or_else(|| {
                        route_internal("parallel JSONL source failure lost terminal evidence")
                    })?;
                if !evidence
                    .observed_certificate()
                    .observation()
                    .source()
                    .exact_descriptor_eq(&failure.source)
                    || failure.retained.as_ref() != Some(&evidence.certificate)
                {
                    return Err(route_internal(
                        "parallel JSONL source failure evidence changed identity",
                    ));
                }
                evidence.record_rejections = std::mem::take(&mut failure.record_rejections);
                evidences.push(evidence);
            }
        }
    }
    if !failed_evidences.is_empty() {
        return Err(route_internal(
            "parallel JSONL terminal evidence has no source failure outcome",
        ));
    }
    Ok(evidences)
}

pub(super) fn terminal_byte_remainder(
    certificate: &CertifiedSource,
    emitted_bytes: u64,
) -> SourceBackedRouteResult<u64> {
    certificate
        .counts()
        .certified_bytes
        .checked_sub(emitted_bytes)
        .ok_or_else(|| {
            route_invalid("JSONL page byte progress exceeded terminal certified source bytes")
        })
}
