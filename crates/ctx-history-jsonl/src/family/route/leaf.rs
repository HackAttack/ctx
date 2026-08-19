use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CertifiedSource, CertifiedSourceAppend};

use super::super::{
    JsonlFamilyError, JsonlFamilyRuntime, JsonlProbe, JsonlReader, JsonlResult, JsonlRuntimeError,
    JsonlRuntimeLookup, JsonlSemanticPreflightMode, JsonlSourceChange, OpenedProviderSourceFile,
};
use super::scanner::{
    map_parallel_leaf_error, physical_identity, preserve_coordinator_error,
    preserve_parallel_emit_error,
};
use super::{
    binding_digest, contract_error, route_internal, route_invalid, route_scan, FamilyCheckpoint,
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyExecutionIo, JsonlFamilyLeaf,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode, JsonlFamilyPublication,
    JsonlFamilySemanticExecutor, JsonlFamilySemanticPreflight, JsonlFamilyTerminalProof,
    JsonlFamilyWorkerContext,
};
mod checkpoint;
mod evidence;
mod output;
mod prepare;
mod semantic;
#[cfg(test)]
use super::{
    jsonl_family_scanner_probe, record_jsonl_family_scanner_activity, JsonlFamilyScannerProbe,
};
#[cfg(any(test, feature = "test-support"))]
pub use checkpoint::checkpoint_admitted_revision_for_test;
pub(super) use checkpoint::decode_checkpoint;
use checkpoint::{certify, fit_semantic_provider_checkpoint, terminal_proof_for_checkpoint};
use ctx_history_capture_runtime::{
    CaptureLifecycleSink, CorePreparedBatchBuilder, ParallelLeafScanBegin,
    ParallelLeafScanComplete, ParallelLeafScanJob, ParallelLeafScanWorkerError,
    SourceBackedGenerationSink, SourceBackedRecordRejectionDrafts, SourceBackedRouteResult,
};
pub(super) use evidence::TerminalSourceEvidence;
use evidence::{
    candidate_would_replace_retained_records_with_only_rejections,
    reconcile_parallel_source_outcomes, terminal_byte_remainder,
};
pub(super) use output::{JsonlLeafOutput, JsonlLeafOutputEvent};
#[cfg(test)]
pub(super) use prepare::prepare_leaf;
use prepare::prepare_leaf_with_resources;
use semantic::{prepare_semantic_leaf, SemanticLeafExecution, SemanticLeafPlan};

const JSONL_SINGLE_LEAF_PIPELINE_MIN_BYTES: u64 = 1024 * 1024;

pub(super) struct PreparedLeaf<E: JsonlFamilyError> {
    pub(super) certificate: CertifiedSource,
    pub(super) append: Option<CertifiedSourceAppend>,
    pub(super) terminal_proof: JsonlFamilyTerminalProof<E>,
    pub(super) record_rejections: SourceBackedRecordRejectionDrafts,
}

struct JsonlLeafJob<E: JsonlFamilyError> {
    leaf: JsonlFamilyLeaf<E>,
    base: Option<CertifiedSource>,
    context_shard: Option<u64>,
}

const JSONL_PARTITION_CONTEXT_SHARDS: usize = 16;
const JSONL_PARTITION_COMPONENTS_PER_WAVE: usize = 16;

// Partitioned adapters receive deterministic logical cache lanes rather than
// caches tied to the physical worker count. Source-local event-time state is
// cleared by `begin_leaf()`, while revalidated repository certification caches
// remain stable across worker counts and physical scheduling decisions.
struct JsonlFamilyWorkerContexts<R: JsonlFamilyRuntime> {
    independent: JsonlFamilyWorkerContext<R>,
    partition_cache_lanes: BTreeMap<u64, JsonlFamilyWorkerContext<R>>,
}

impl<R: JsonlFamilyRuntime> Default for JsonlFamilyWorkerContexts<R> {
    fn default() -> Self {
        Self {
            independent: JsonlFamilyWorkerContext::default(),
            partition_cache_lanes: BTreeMap::new(),
        }
    }
}

impl<R: JsonlFamilyRuntime> JsonlFamilyWorkerContexts<R> {
    fn for_job(&mut self, context_shard: Option<u64>) -> &mut JsonlFamilyWorkerContext<R> {
        match context_shard {
            Some(context_shard) => self.partition_cache_lanes.entry(context_shard).or_default(),
            None => &mut self.independent,
        }
    }
}

fn scan_leaf_serial<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: Option<&CertifiedSource>,
    base_event_lookup: &JsonlRuntimeLookup<R>,
    worker: &mut JsonlFamilyWorkerContext<R>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
    append_only_trust_allowed: bool,
) -> SourceBackedRouteResult<TerminalSourceEvidence<JsonlRuntimeError<R>>> {
    let mut staging_started = false;
    let mut append_staging = false;
    let mut sink_failure = None;
    let mut emitted_bytes = 0_u64;
    let route_resources = sink.route_resources();
    let mut emit = |event| {
        if matches!(
            &event,
            JsonlLeafOutputEvent::Page { records, .. } if records.is_empty()
        ) {
            return Ok(());
        }
        let append = match &event {
            JsonlLeafOutputEvent::Page { append, .. }
            | JsonlLeafOutputEvent::Record { append, .. } => *append,
            JsonlLeafOutputEvent::Flush => return Ok(()),
        };
        if !staging_started {
            if append {
                let expected = base.ok_or_else(|| {
                    JsonlRuntimeError::<R>::invalid_payload("JSONL append has no base".to_owned())
                })?;
                let staged = sink
                    .begin_source_append(leaf.source().clone())
                    .map_err(|error| preserve_coordinator_error::<R>(&mut sink_failure, error))?;
                if staged != expected {
                    return Err(JsonlRuntimeError::<R>::invalid_payload(
                        "JSONL append base changed before staging".to_owned(),
                    ));
                }
            } else {
                sink.begin_source(leaf.source().clone())
                    .map_err(|error| preserve_coordinator_error::<R>(&mut sink_failure, error))?;
            }
            staging_started = true;
            append_staging = append;
        } else if append_staging != append {
            return Err(JsonlRuntimeError::<R>::system_invariant(
                "JSONL publication mode changed during one leaf scan",
            ));
        }
        match event {
            JsonlLeafOutputEvent::Page {
                completed_bytes,
                records,
                ..
            } => {
                sink.add_core_records_with_completed_bytes(records, completed_bytes)
                    .map_err(|error| preserve_coordinator_error::<R>(&mut sink_failure, error))?;
                emitted_bytes = emitted_bytes.checked_add(completed_bytes).ok_or_else(|| {
                    JsonlRuntimeError::<R>::system_invariant(
                        "JSONL emitted source-byte progress overflowed",
                    )
                })?;
            }
            JsonlLeafOutputEvent::Record { record, .. } => {
                sink.add_core_record(record)
                    .map_err(|error| preserve_coordinator_error::<R>(&mut sink_failure, error))?;
            }
            JsonlLeafOutputEvent::Flush => unreachable!("flush returned before staging"),
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let prepared = prepare_leaf_with_resources(
        adapter,
        leaf,
        base,
        base_event_lookup,
        worker,
        &mut output,
        append_only_trust_allowed,
        &route_resources,
    );
    if let Some(error) = sink_failure {
        return Err(error);
    }
    let prepared = prepared.map_err(|error| route_scan(adapter, error))?;

    let PreparedLeaf {
        certificate,
        append,
        terminal_proof,
        record_rejections,
    } = prepared;
    if candidate_would_replace_retained_records_with_only_rejections(&certificate, base) {
        let retained = base.cloned();
        let publication_certificate = retained.clone().unwrap_or_else(|| certificate.clone());
        if let Some(base) = retained.as_ref() {
            sink.retain_source(base.clone()).map_err(route_internal)?;
        }
        sink.record_logical_source_failure(
            leaf.source().clone(),
            route_invalid("JSONL source is unreadable"),
            retained.is_some(),
        )
        .map_err(route_internal)?;
        sink.record_rejections(record_rejections);
        sink.report_completed_bytes_with_exact(
            certificate.counts().certified_bytes,
            leaf.frozen_scan_observation()
                .map(|observation| observation.length()),
        )
        .map_err(route_internal)?;
        return Ok(TerminalSourceEvidence {
            certificate: publication_certificate,
            terminal_certificate: Some(certificate),
            terminal_proof,
            emitted_bytes: 0,
            exact_scan_bytes: leaf
                .frozen_scan_observation()
                .map(|observation| observation.length()),
            record_rejections: SourceBackedRecordRejectionDrafts::default(),
        });
    }
    sink.record_rejections(record_rejections);
    match append {
        Some(append) => {
            if staging_started && !append_staging {
                return Err(route_internal(
                    "append JSONL source emitted replacement documents",
                ));
            }
            if !staging_started {
                let staged = sink
                    .begin_source_append(leaf.source().clone())
                    .map_err(route_internal)?;
                if staged != append.base() {
                    return Err(route_invalid("JSONL append base changed before staging"));
                }
            }
            sink.certify_source_append(append).map_err(route_internal)?;
            sink.report_completed_bytes_with_exact(
                terminal_byte_remainder(&certificate, emitted_bytes)?,
                leaf.frozen_scan_observation()
                    .and_then(|observation| observation.length().checked_sub(emitted_bytes)),
            )
            .map_err(route_internal)?;
            Ok(TerminalSourceEvidence {
                certificate,
                terminal_certificate: None,
                terminal_proof,
                emitted_bytes,
                exact_scan_bytes: leaf
                    .frozen_scan_observation()
                    .map(|observation| observation.length()),
                record_rejections: SourceBackedRecordRejectionDrafts::default(),
            })
        }
        None => {
            if staging_started && append_staging {
                return Err(route_internal(
                    "replacement JSONL source emitted append documents",
                ));
            }
            if !staging_started {
                sink.begin_source(leaf.source().clone())
                    .map_err(route_internal)?;
            }
            sink.certify_source(certificate.clone())
                .map_err(route_internal)?;
            sink.report_completed_bytes_with_exact(
                terminal_byte_remainder(&certificate, emitted_bytes)?,
                leaf.frozen_scan_observation()
                    .and_then(|observation| observation.length().checked_sub(emitted_bytes)),
            )
            .map_err(route_internal)?;
            Ok(TerminalSourceEvidence {
                certificate,
                terminal_certificate: None,
                terminal_proof,
                emitted_bytes,
                exact_scan_bytes: leaf
                    .frozen_scan_observation()
                    .map(|observation| observation.length()),
                record_rejections: SourceBackedRecordRejectionDrafts::default(),
            })
        }
    }
}

fn run_parallel_leaf_job_batch<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    jobs: Vec<ParallelLeafScanJob<JsonlLeafJob<JsonlRuntimeError<R>>>>,
    worker_states: &mut [JsonlFamilyWorkerContexts<R>],
    base_event_lookup: &JsonlRuntimeLookup<R>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
    append_only_trust_allowed: bool,
    #[cfg(test)] scanner_probe: Option<&JsonlFamilyScannerProbe>,
) -> SourceBackedRouteResult<Vec<TerminalSourceEvidence<JsonlRuntimeError<R>>>> {
    let failed_evidences = Mutex::new(Vec::new());
    let result = sink.run_parallel_leaf_scans_with_worker_states_and_source_outcomes(
        jobs,
        worker_states,
        |contexts, job, emitter| {
            let worker = contexts.for_job(job.leaf().context_shard);
            #[cfg(test)]
            let _active_scanner = scanner_probe.map(JsonlFamilyScannerProbe::enter);
            let leaf = &job.leaf().leaf;
            let mut staging_started = false;
            let mut append_staging = false;
            let mut emission_failure = None;
            let mut pending_emissions = CorePreparedBatchBuilder::<
                <R::Lifecycle as CaptureLifecycleSink>::Preparation,
            >::default();
            let mut emitted_bytes = 0_u64;
            let route_resources = emitter.route_resources();
            let mut emit = |event| {
                if matches!(
                    &event,
                    JsonlLeafOutputEvent::Page { records, .. } if records.is_empty()
                ) {
                    return Ok(());
                }
                let flush = matches!(
                    &event,
                    JsonlLeafOutputEvent::Page { .. } | JsonlLeafOutputEvent::Flush
                );
                let append = match &event {
                    JsonlLeafOutputEvent::Page { append, .. }
                    | JsonlLeafOutputEvent::Record { append, .. } => Some(*append),
                    JsonlLeafOutputEvent::Flush => None,
                };
                let completed_bytes = match &event {
                    JsonlLeafOutputEvent::Page {
                        completed_bytes, ..
                    } => *completed_bytes,
                    JsonlLeafOutputEvent::Record { .. } | JsonlLeafOutputEvent::Flush => 0,
                };
                if let Some(append) = append {
                    if !staging_started {
                        let begin = if append {
                            let base = job.leaf().base.clone().ok_or_else(|| {
                                JsonlRuntimeError::<R>::invalid_payload(
                                    "parallel JSONL append has no base".to_owned(),
                                )
                            })?;
                            ParallelLeafScanBegin::append(leaf.source().clone(), base)
                        } else {
                            ParallelLeafScanBegin::replace(leaf.source().clone())
                        };
                        emitter.begin(begin).map_err(|_| {
                            JsonlRuntimeError::<R>::system_invariant(
                                "JSONL parallel scan was cancelled before publication",
                            )
                        })?;
                        staging_started = true;
                        append_staging = append;
                    } else if append_staging != append {
                        return Err(JsonlRuntimeError::<R>::system_invariant(
                            "parallel JSONL publication mode changed during one leaf scan",
                        ));
                    }
                    match event {
                        JsonlLeafOutputEvent::Page { records, .. } => {
                            emitter
                                .emit_core_records_with_completed_bytes(
                                    &mut pending_emissions,
                                    records,
                                    completed_bytes,
                                )
                                .map_err(|error| {
                                    preserve_parallel_emit_error::<JsonlRuntimeError<R>>(
                                        &mut emission_failure,
                                        error,
                                    )
                                })?;
                        }
                        JsonlLeafOutputEvent::Record { record, .. } => {
                            emitter
                                .emit_core_record_batched(&mut pending_emissions, record)
                                .map_err(|error| {
                                    preserve_parallel_emit_error::<JsonlRuntimeError<R>>(
                                        &mut emission_failure,
                                        error,
                                    )
                                })?;
                        }
                        JsonlLeafOutputEvent::Flush => {
                            unreachable!("flush has no publication mode")
                        }
                    }
                }
                if flush && append.is_none() {
                    emitter
                        .emit_core_record_batch(&mut pending_emissions)
                        .map_err(|error| {
                            preserve_parallel_emit_error::<JsonlRuntimeError<R>>(
                                &mut emission_failure,
                                error,
                            )
                        })?;
                }
                if completed_bytes != 0 {
                    emitted_bytes =
                        emitted_bytes.checked_add(completed_bytes).ok_or_else(|| {
                            JsonlRuntimeError::<R>::system_invariant(
                                "parallel JSONL emitted source-byte progress overflowed",
                            )
                        })?;
                }
                Ok(())
            };
            let mut output = JsonlLeafOutput::new(&mut emit);
            let prepared = prepare_leaf_with_resources(
                adapter,
                leaf,
                job.leaf().base.as_ref(),
                base_event_lookup,
                worker,
                &mut output,
                append_only_trust_allowed,
                &route_resources,
            );
            if let Some(error) = emission_failure {
                return Err(ParallelLeafScanWorkerError::provider(error));
            }
            let prepared = prepared
                .map_err(|error| route_scan(adapter, error))
                .map_err(ParallelLeafScanWorkerError::provider)?;

            let PreparedLeaf {
                certificate,
                append,
                terminal_proof,
                record_rejections,
            } = prepared;
            if candidate_would_replace_retained_records_with_only_rejections(
                &certificate,
                job.leaf().base.as_ref(),
            ) {
                if staging_started || append.is_some() {
                    return Err(ParallelLeafScanWorkerError::provider(route_internal(
                        "all-rejected JSONL leaf entered publication staging",
                    )));
                }
                let retained = job.leaf().base.clone();
                let publication_certificate =
                    retained.clone().unwrap_or_else(|| certificate.clone());
                failed_evidences
                    .lock()
                    .map_err(|_| {
                        ParallelLeafScanWorkerError::provider(route_internal(
                            "JSONL failed-evidence lock was poisoned",
                        ))
                    })?
                    .push(TerminalSourceEvidence {
                        certificate: publication_certificate,
                        terminal_certificate: Some(certificate),
                        terminal_proof,
                        emitted_bytes: 0,
                        exact_scan_bytes: leaf
                            .frozen_scan_observation()
                            .map(|observation| observation.length()),
                        record_rejections: SourceBackedRecordRejectionDrafts::default(),
                    });
                emitter
                    .complete(ParallelLeafScanComplete::source_failure_with_rejections(
                        leaf.source().clone(),
                        retained,
                        route_invalid("JSONL source is unreadable"),
                        record_rejections,
                    ))
                    .map_err(ParallelLeafScanWorkerError::from)?;
                return Ok(());
            }
            match append {
                Some(append) => {
                    if staging_started && !append_staging {
                        return Err(ParallelLeafScanWorkerError::provider(route_invalid(
                            "parallel JSONL append emitted replacement documents",
                        )));
                    }
                    if !staging_started {
                        emitter
                            .begin(ParallelLeafScanBegin::append(
                                leaf.source().clone(),
                                append.base().clone(),
                            ))
                            .map_err(ParallelLeafScanWorkerError::from)?;
                    }
                    emitter
                        .complete(ParallelLeafScanComplete::append(
                            append,
                            TerminalSourceEvidence {
                                certificate,
                                terminal_certificate: None,
                                terminal_proof,
                                emitted_bytes,
                                exact_scan_bytes: leaf
                                    .frozen_scan_observation()
                                    .map(|observation| observation.length()),
                                record_rejections,
                            },
                        ))
                        .map_err(ParallelLeafScanWorkerError::from)?;
                }
                None => {
                    if staging_started && append_staging {
                        return Err(ParallelLeafScanWorkerError::provider(route_invalid(
                            "parallel JSONL replacement emitted append documents",
                        )));
                    }
                    if !staging_started {
                        emitter
                            .begin(ParallelLeafScanBegin::replace(leaf.source().clone()))
                            .map_err(ParallelLeafScanWorkerError::from)?;
                    }
                    let evidence = TerminalSourceEvidence {
                        certificate: certificate.clone(),
                        terminal_certificate: None,
                        terminal_proof,
                        emitted_bytes,
                        exact_scan_bytes: leaf
                            .frozen_scan_observation()
                            .map(|observation| observation.length()),
                        record_rejections,
                    };
                    emitter
                        .complete(ParallelLeafScanComplete::replace(certificate, evidence))
                        .map_err(ParallelLeafScanWorkerError::from)?;
                }
            }
            Ok(())
        },
    );
    let outcomes = result.map_err(map_parallel_leaf_error)?;
    let failed_evidences = failed_evidences
        .into_inner()
        .map_err(|_| route_internal("JSONL failed-evidence lock was poisoned"))?;
    let evidences = reconcile_parallel_source_outcomes(outcomes, failed_evidences)?;
    for evidence in &evidences {
        sink.record_rejections(evidence.record_rejections.clone());
        sink.report_completed_bytes_with_exact(
            terminal_byte_remainder(evidence.observed_certificate(), evidence.emitted_bytes)?,
            evidence
                .exact_scan_bytes
                .and_then(|total| total.checked_sub(evidence.emitted_bytes)),
        )
        .map_err(route_internal)?;
    }
    Ok(evidences)
}

pub(super) fn scan_leaves<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaves: &[JsonlFamilyLeaf<JsonlRuntimeError<R>>],
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    base_event_lookup: JsonlRuntimeLookup<R>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
    append_only_trust_allowed: bool,
) -> SourceBackedRouteResult<HashMap<[u8; 32], TerminalSourceEvidence<JsonlRuntimeError<R>>>> {
    let worker_limit = adapter
        .prepare_leaf_scans(leaves, bases)
        .map_err(|error| route_scan(adapter, error))?;
    let recommended_workers = sink.recommended_leaf_workers(leaves.len());
    let recommended_workers = worker_limit
        .map(|limit| recommended_workers.min(limit.max(1)))
        .unwrap_or(recommended_workers);
    let worker_count = family_scanner_worker_count(recommended_workers);
    let mut leaf_metadata = Vec::new();
    leaf_metadata
        .try_reserve_exact(leaves.len())
        .map_err(|_| route_internal("JSONL leaf scheduling allocation failed"))?;
    let mut saw_partition = false;
    let mut saw_unpartitioned = false;
    let mut previous_phase = None;
    for leaf in leaves {
        let phase = adapter
            .leaf_scan_phase(leaf)
            .map_err(|error| route_scan(adapter, error))?;
        if previous_phase.is_some_and(|previous| previous > phase) {
            return Err(route_invalid(
                "JSONL adapter returned non-monotonic leaf scan phases",
            ));
        }
        previous_phase = Some(phase);
        let partition = adapter
            .leaf_scan_partition(leaf)
            .map_err(|error| route_scan(adapter, error))?;
        saw_partition |= partition.is_some();
        saw_unpartitioned |= partition.is_none();
        leaf_metadata.push((phase, partition));
    }
    if saw_partition && saw_unpartitioned {
        return Err(route_invalid(
            "JSONL adapter mixed partitioned and unpartitioned leaf scans",
        ));
    }
    let partition_wave_limit = adapter
        .leaf_scan_partition_wave_limit()
        .min(JSONL_PARTITION_COMPONENTS_PER_WAVE);
    if saw_partition && partition_wave_limit == 0 {
        return Err(route_invalid(
            "JSONL adapter returned a zero partition wave limit",
        ));
    }
    let mut serial_worker = JsonlFamilyWorkerContext::default();
    #[cfg(test)]
    let scanner_probe = jsonl_family_scanner_probe(if saw_partition { 1 } else { worker_count });
    // Pipeline multi-leaf families and large single files so scanning can
    // overlap writer admission even when concurrency is capped at one.
    let large_single_leaf = leaves.len() == 1
        && leaves.first().is_some_and(|leaf| {
            leaf.estimated_scan_bytes() >= JSONL_SINGLE_LEAF_PIPELINE_MIN_BYTES
        });
    if worker_count <= 1 && leaves.len() <= 1 && !large_single_leaf {
        let mut terminal_sources = HashMap::with_capacity(leaves.len());
        for (leaf_index, leaf) in leaves.iter().enumerate() {
            let partition = leaf_metadata
                .get(leaf_index)
                .and_then(|(_, partition)| *partition);
            if let Some(partition) = partition {
                adapter
                    .begin_leaf_scan_partition(partition)
                    .map_err(|error| route_scan(adapter, error))?;
            }
            #[cfg(test)]
            let _active_scanner = scanner_probe.as_ref().map(|probe| probe.enter());
            let evidence = scan_leaf_serial(
                adapter,
                leaf,
                base_for_leaf(bases, leaf),
                &base_event_lookup,
                &mut serial_worker,
                sink,
                append_only_trust_allowed,
            );
            let finish_partition = partition
                .map(|partition| {
                    adapter
                        .finish_leaf_scan_partition(partition)
                        .map_err(|error| route_scan(adapter, error))
                })
                .transpose();
            let evidence = evidence?;
            finish_partition?;
            if terminal_sources
                .insert(leaf.source().exact_descriptor_digest(), evidence)
                .is_some()
            {
                return Err(route_invalid("duplicate JSONL source identity"));
            }
        }
        #[cfg(test)]
        record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());
        return Ok(terminal_sources);
    }

    let mut worker_states = (0..worker_count)
        .map(|_| JsonlFamilyWorkerContexts::default())
        .collect::<Vec<_>>();

    if saw_partition {
        let mut partitions =
            BTreeMap::<u64, Vec<(usize, JsonlFamilyLeaf<JsonlRuntimeError<R>>)>>::new();
        for (leaf, (phase, partition)) in leaves.iter().cloned().zip(leaf_metadata.iter()) {
            let partition = partition.ok_or_else(|| {
                route_invalid("JSONL partition metadata disappeared before scheduling")
            })?;
            partitions
                .entry(partition)
                .or_default()
                .push((*phase, leaf));
        }
        let mut partitions = partitions.into_iter().collect::<Vec<_>>();
        partitions.sort_by(
            |(left_partition, left_leaves), (right_partition, right_leaves)| {
                let left_bytes = left_leaves.iter().fold(0_u64, |total, (_, leaf)| {
                    total.saturating_add(leaf.estimated_scan_bytes())
                });
                let right_bytes = right_leaves.iter().fold(0_u64, |total, (_, leaf)| {
                    total.saturating_add(leaf.estimated_scan_bytes())
                });
                right_bytes
                    .cmp(&left_bytes)
                    .then_with(|| left_partition.cmp(right_partition))
            },
        );
        let mut evidences = Vec::with_capacity(leaves.len());
        for wave in partitions.chunks(partition_wave_limit) {
            let mut begun = Vec::with_capacity(wave.len());
            for (partition, _) in wave {
                if let Err(error) = adapter.begin_leaf_scan_partition(*partition) {
                    for begun_partition in begun.into_iter().rev() {
                        let _ = adapter.finish_leaf_scan_partition(begun_partition);
                    }
                    return Err(route_scan(adapter, error));
                }
                begun.push(*partition);
            }

            let mut frontiers =
                BTreeMap::<usize, Vec<JsonlFamilyLeaf<JsonlRuntimeError<R>>>>::new();
            for (_, partition_leaves) in wave {
                for (phase, leaf) in partition_leaves {
                    frontiers.entry(*phase).or_default().push(leaf.clone());
                }
            }

            let batch: SourceBackedRouteResult<Vec<TerminalSourceEvidence<JsonlRuntimeError<R>>>> =
                (|| {
                    let mut batch = Vec::new();
                    for (_, mut frontier) in frontiers {
                        frontier.sort_by(|left, right| {
                            right
                                .estimated_scan_bytes()
                                .cmp(&left.estimated_scan_bytes())
                                .then_with(|| {
                                    left.source()
                                        .exact_descriptor_digest()
                                        .cmp(&right.source().exact_descriptor_digest())
                                })
                        });
                        let logical_lane_count = JSONL_PARTITION_CONTEXT_SHARDS.min(frontier.len());
                        let mut lane_bytes = vec![0_u64; logical_lane_count];
                        let mut jobs = Vec::with_capacity(frontier.len());
                        for leaf in frontier {
                            let lane = lane_bytes
                                .iter()
                                .enumerate()
                                .min_by_key(|(lane, bytes)| (**bytes, *lane))
                                .map(|(lane, _)| lane)
                                .ok_or_else(|| {
                                    route_internal("JSONL frontier has no worker lane")
                                })?;
                            lane_bytes[lane] =
                                lane_bytes[lane].saturating_add(leaf.estimated_scan_bytes());
                            let base = base_for_leaf(bases, &leaf).cloned();
                            jobs.push(
                                ParallelLeafScanJob::new(
                                    leaf.source().clone(),
                                    JsonlLeafJob {
                                        leaf,
                                        base,
                                        context_shard: Some(lane as u64),
                                    },
                                )
                                .with_worker_affinity(lane as u64),
                            );
                        }
                        batch.extend(run_parallel_leaf_job_batch(
                            adapter,
                            jobs,
                            &mut worker_states,
                            &base_event_lookup,
                            sink,
                            append_only_trust_allowed,
                            #[cfg(test)]
                            scanner_probe.as_deref(),
                        )?);
                    }
                    Ok(batch)
                })();
            let mut finish_error = None;
            for partition in begun.into_iter().rev() {
                if let Err(error) = adapter.finish_leaf_scan_partition(partition) {
                    if finish_error.is_none() {
                        finish_error = Some(route_scan(adapter, error));
                    }
                }
            }
            let batch = batch?;
            if let Some(error) = finish_error {
                return Err(error);
            }
            evidences.extend(batch);
        }
        #[cfg(test)]
        record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());
        return collect_terminal_sources(evidences);
    }

    let phases = leaf_metadata
        .iter()
        .map(|(phase, _)| *phase)
        .collect::<Vec<_>>();

    let mut evidences = Vec::with_capacity(leaves.len());
    let mut phase_start = 0_usize;
    while phase_start < leaves.len() {
        let phase = phases[phase_start];
        let mut phase_end = phase_start.saturating_add(1);
        while phase_end < leaves.len() && phases[phase_end] == phase {
            phase_end = phase_end.saturating_add(1);
        }
        let mut jobs = Vec::with_capacity(phase_end.saturating_sub(phase_start));
        for leaf in leaves[phase_start..phase_end].iter().cloned() {
            let base = base_for_leaf(bases, &leaf).cloned();
            let worker_affinity = adapter
                .leaf_worker_affinity(&leaf)
                .map_err(|error| route_scan(adapter, error))?;
            let job = ParallelLeafScanJob::new(
                leaf.source().clone(),
                JsonlLeafJob {
                    leaf,
                    base,
                    context_shard: None,
                },
            );
            jobs.push(match worker_affinity {
                Some(worker_affinity) => job.with_worker_affinity(worker_affinity),
                None => job,
            });
        }
        let phase_evidences = run_parallel_leaf_job_batch(
            adapter,
            jobs,
            &mut worker_states,
            &base_event_lookup,
            sink,
            append_only_trust_allowed,
            #[cfg(test)]
            scanner_probe.as_deref(),
        )?;
        evidences.extend(phase_evidences);
        phase_start = phase_end;
    }
    #[cfg(test)]
    record_jsonl_family_scanner_activity(worker_count, scanner_probe.as_deref());

    collect_terminal_sources(evidences)
}

fn collect_terminal_sources<E: JsonlFamilyError>(
    evidences: Vec<TerminalSourceEvidence<E>>,
) -> SourceBackedRouteResult<HashMap<[u8; 32], TerminalSourceEvidence<E>>> {
    let mut terminal_sources = HashMap::with_capacity(evidences.len());
    for evidence in evidences {
        let digest = evidence
            .certificate
            .observation()
            .source()
            .exact_descriptor_digest();
        if terminal_sources.insert(digest, evidence).is_some() {
            return Err(route_invalid("duplicate JSONL source identity"));
        }
    }
    Ok(terminal_sources)
}

fn open_leaf_reader<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    opened: &Arc<OpenedProviderSourceFile<JsonlRuntimeError<R>>>,
    previous: Option<&FamilyCheckpoint>,
    projector_preflight: bool,
    append_only_trust_allowed: bool,
    route_resources: &ctx_history_capture_runtime::SourceBackedRouteResources,
) -> JsonlResult<JsonlReader<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    let direct_append = previous
        .and_then(|checkpoint| checkpoint.provider_checkpoint.as_ref())
        .is_some_and(|checkpoint| {
            append_only_trust_allowed
                && adapter.append_trust_contract()
                    == super::JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
                && adapter.allows_direct_append_for_leaf(leaf)
                && adapter.accepts_direct_append_checkpoint(checkpoint)
        });
    let mut reader = if leaf.whole_record {
        JsonlReader::open_whole_record(
            physical_identity(adapter, leaf),
            Arc::clone(opened),
            previous.map(|checkpoint| &checkpoint.physical),
        )
    } else {
        if adapter.bind_admitted_eof() {
            JsonlReader::open_semantic_with_record_framing_and_encoding_direct_and_resources(
                physical_identity(adapter, leaf),
                Arc::clone(opened),
                previous.map(|checkpoint| &checkpoint.physical),
                JsonlSemanticPreflightMode::AdmittedEof(
                    previous.and_then(|checkpoint| checkpoint.admitted_eof_sha256),
                ),
                leaf.identity_probe.clone(),
                adapter.physical_encoding(leaf),
                adapter.record_framing(),
                leaf.frozen_scan_observation(),
                direct_append,
                Some(route_resources),
            )
        } else if projector_preflight {
            JsonlReader::open_semantic_with_record_framing_and_encoding_direct_and_resources(
                physical_identity(adapter, leaf),
                Arc::clone(opened),
                previous.map(|checkpoint| &checkpoint.physical),
                JsonlSemanticPreflightMode::CompletePrefix,
                None,
                adapter.physical_encoding(leaf),
                adapter.record_framing(),
                leaf.frozen_scan_observation(),
                direct_append,
                Some(route_resources),
            )
        } else {
            JsonlReader::open_with_record_framing_and_encoding_and_resources(
                physical_identity(adapter, leaf),
                Arc::clone(opened),
                previous.map(|checkpoint| &checkpoint.physical),
                leaf.identity_probe.clone(),
                adapter.physical_encoding(leaf),
                adapter.record_framing(),
                route_resources,
            )
        }
    }?;
    reader.set_oversized_record_policy(adapter.oversized_record_policy());
    Ok(reader)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_optimized_outcome<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: Option<&CertifiedSource>,
    outcome: JsonlFamilyOptimizedLeafOutcome<JsonlRuntimeError<R>>,
) -> JsonlResult<PreparedLeaf<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    outcome
        .certificate
        .validate_contract()
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    leaf.source()
        .validate_exact_descriptor(outcome.certificate.observation().source())
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    if outcome.certificate.parser_revision() != adapter.parser_revision() {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "optimized JSONL leaf changed the parser revision".to_owned(),
        ));
    }
    if let Some(append) = outcome.append.as_ref() {
        let base = base.ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("optimized JSONL append has no base".to_owned())
        })?;
        if append.base() != base || append.current() != &outcome.certificate {
            return Err(JsonlRuntimeError::<R>::invalid_payload(
                "optimized JSONL append evidence does not reconcile".to_owned(),
            ));
        }
    }
    outcome
        .terminal_proof
        .validate_for(adapter, leaf, &outcome.certificate)?;
    Ok(PreparedLeaf {
        certificate: outcome.certificate,
        append: outcome.append,
        terminal_proof: outcome.terminal_proof,
        record_rejections: SourceBackedRecordRejectionDrafts::default(),
    })
}

pub(super) fn base_for_leaf<'a, E: JsonlFamilyError>(
    bases: &'a HashMap<[u8; 32], &CertifiedSource>,
    leaf: &JsonlFamilyLeaf<E>,
) -> Option<&'a CertifiedSource> {
    bases
        .get(&leaf.source().exact_descriptor_digest())
        .copied()
        .filter(|base| {
            base.observation()
                .source()
                .exact_descriptor_eq(leaf.source())
        })
}

pub(super) fn family_scanner_worker_count_policy(
    recommended: usize,
    requested_workers: Option<usize>,
) -> usize {
    if recommended == 0 {
        return 0;
    }
    requested_workers
        .unwrap_or(recommended)
        .clamp(1, recommended)
}

fn family_scanner_worker_count(recommended: usize) -> usize {
    #[cfg(test)]
    {
        super::FAMILY_SCANNER_WORKERS_OVERRIDE.with(|value| {
            family_scanner_worker_count_policy(recommended, Some(value.get().unwrap_or(1)))
        })
    }
    #[cfg(not(test))]
    {
        family_scanner_worker_count_policy(recommended, None)
    }
}

fn checked_increment<E: JsonlFamilyError>(value: u64) -> JsonlResult<u64, E> {
    value
        .checked_add(1)
        .ok_or_else(|| E::system_invariant("JSONL work counter overflowed"))
}
