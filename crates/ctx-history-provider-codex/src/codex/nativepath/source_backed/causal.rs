use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

use super::super::source_backed::CodexSourceBackedCountersV0;
use super::catalog::CodexCatalogWorkV0;
use super::*;

const RECEIPT_ENV: &str = "CTX_CODEX_CAUSAL_RECEIPT";
const CANDIDATE_SHA_ENV: &str = "CTX_CODEX_CAUSAL_CANDIDATE_SHA";
const WORKLOAD_ENV: &str = "CTX_CODEX_CAUSAL_WORKLOAD_ID";
const APPEND_FIXTURE_SHA_ENV: &str = "CTX_CODEX_CAUSAL_APPEND_FIXTURE_SHA256";
const DESCENDANT_MANIFEST_SHA_ENV: &str = "CTX_CODEX_CAUSAL_DESCENDANT_MANIFEST_SHA256";
const WORKLOAD_ID: &str = "ctx306-child-independent-hydration-v1";
const TARGET_PROVIDER_SESSION_ID: &str = "019f3b0d-6ace-7f31-af6e-b63d68b5bdfe";
#[derive(Debug, Default)]
pub(super) struct CodexCausalLedgerV1 {
    sources: BTreeMap<String, CodexCausalSourceV1>,
}

#[derive(Debug, Default)]
struct CodexCausalSourceV1 {
    counters: CodexSourceBackedCountersV0,
}

impl CodexCausalLedgerV1 {
    pub(super) fn observe_catalog(
        &mut self,
        provider_session_id: &str,
        work: CodexCatalogWorkV0,
        exact_replay: bool,
    ) {
        let source = self
            .sources
            .entry(provider_session_id.to_owned())
            .or_default();
        source.counters.add_catalog_work(work);
        source.counters.writer_exact_replay_sources = source
            .counters
            .writer_exact_replay_sources
            .saturating_add(u64::from(exact_replay));
    }

    pub(super) fn observe_scan(
        &mut self,
        provider_session_id: &str,
        counters: CodexSourceBackedCountersV0,
    ) {
        self.sources
            .entry(provider_session_id.to_owned())
            .or_default()
            .counters
            .add_assign(counters);
    }

    pub(super) fn write_qualification_receipt(&self) -> CodexSourceBackedResultV0<()> {
        let Some(config) = CausalReceiptConfigV1::from_environment()? else {
            return Ok(());
        };
        let Some(parent) = self.sources.get(TARGET_PROVIDER_SESSION_ID) else {
            return Ok(());
        };
        let parent_changed = parent
            .counters
            .appended_sources
            .saturating_add(parent.counters.replaced_sources)
            .saturating_add(parent.counters.cold_sources);
        if parent_changed == 0 {
            return Ok(());
        }
        let physical_attempt_id = current_physical_attempt_id()?;
        let descendants = self
            .sources
            .iter()
            // The qualification route and descendant manifest already bound
            // this workload. Do not reopen unchanged provider bodies merely to
            // reconstruct a second test-only copy of their Core lineage.
            .filter(|(provider_session_id, _)| {
                provider_session_id.as_str() != TARGET_PROVIDER_SESSION_ID
            })
            .map(|(provider_session_id, source)| {
                CodexCausalDescendantReceiptV1::new(provider_session_id.clone(), source.counters)
            })
            .collect();
        let receipt = CodexCausalReceiptV1 {
            schema_version: 1,
            workload_id: WORKLOAD_ID,
            candidate_source_sha: config.candidate_source_sha,
            physical_attempt_id,
            append_fixture_sha256: config.append_fixture_sha256,
            descendant_manifest_sha256: config.descendant_manifest_sha256,
            target_provider_session_id: TARGET_PROVIDER_SESSION_ID,
            parent: CodexCausalParentReceiptV1::new(parent.counters),
            descendants,
        };
        write_atomic_json(&config.path, &receipt)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn run_test_observer(&self) {
        let sources = self
            .sources
            .iter()
            .map(
                |(provider_session_id, source)| CodexCausalSourceObservationV1 {
                    provider_session_id: provider_session_id.clone(),
                    counters: source.counters,
                },
            )
            .collect();
        AFTER_CODEX_CAUSAL_STAGE_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook(sources);
            }
        });
    }
}

#[cfg(any(test, feature = "test-support", ctx_codex_causal_qualification))]
#[derive(Debug, Clone)]
pub struct CodexCausalSourceObservationV1 {
    pub provider_session_id: String,
    pub counters: CodexSourceBackedCountersV0,
}

#[cfg(any(test, feature = "test-support", ctx_codex_causal_qualification))]
type AfterCodexCausalStageHook = Option<Box<dyn FnOnce(Vec<CodexCausalSourceObservationV1>)>>;

#[cfg(any(test, feature = "test-support", ctx_codex_causal_qualification))]
std::thread_local! {
    static AFTER_CODEX_CAUSAL_STAGE_HOOK: std::cell::RefCell<AfterCodexCausalStageHook> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support", ctx_codex_causal_qualification))]
pub fn install_after_codex_causal_stage_hook_v1(
    hook: impl FnOnce(Vec<CodexCausalSourceObservationV1>) + 'static,
) {
    AFTER_CODEX_CAUSAL_STAGE_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex causal stage hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

struct CausalReceiptConfigV1 {
    path: PathBuf,
    candidate_source_sha: String,
    append_fixture_sha256: String,
    descendant_manifest_sha256: String,
}

impl CausalReceiptConfigV1 {
    fn from_environment() -> CodexSourceBackedResultV0<Option<Self>> {
        let Ok(workload_id) = std::env::var(WORKLOAD_ENV) else {
            return Ok(None);
        };
        if workload_id != WORKLOAD_ID {
            return Ok(None);
        }
        let required = |name: &'static str| {
            std::env::var(name).map_err(|_| {
                CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(format!(
                    "Codex causal qualification environment is missing {name}"
                )))
            })
        };
        Ok(Some(Self {
            path: PathBuf::from(required(RECEIPT_ENV)?),
            candidate_source_sha: required(CANDIDATE_SHA_ENV)?,
            append_fixture_sha256: required(APPEND_FIXTURE_SHA_ENV)?,
            descendant_manifest_sha256: required(DESCENDANT_MANIFEST_SHA_ENV)?,
        }))
    }
}

#[derive(Serialize)]
struct CodexCausalReceiptV1 {
    schema_version: u8,
    workload_id: &'static str,
    candidate_source_sha: String,
    physical_attempt_id: String,
    append_fixture_sha256: String,
    descendant_manifest_sha256: String,
    target_provider_session_id: &'static str,
    parent: CodexCausalParentReceiptV1,
    descendants: Vec<CodexCausalDescendantReceiptV1>,
}

#[derive(Serialize)]
struct CodexCausalParentReceiptV1 {
    provider_session_id: &'static str,
    changed_sources: u64,
    appended_sources: u64,
    replaced_sources: u64,
    writer_mutated_sources: u64,
    staged_documents: u64,
    complete_records_scanned: u64,
    retained_records_scanned: u64,
    rejected_records_scanned: u64,
    ignored_records_scanned: u64,
}

impl CodexCausalParentReceiptV1 {
    fn new(counters: CodexSourceBackedCountersV0) -> Self {
        Self {
            provider_session_id: TARGET_PROVIDER_SESSION_ID,
            changed_sources: u64::from(
                counters.appended_sources != 0
                    || counters.replaced_sources != 0
                    || counters.cold_sources != 0,
            ),
            appended_sources: counters.appended_sources,
            replaced_sources: counters.replaced_sources,
            writer_mutated_sources: counters.writer_mutated_sources,
            staged_documents: counters.staged_documents,
            complete_records_scanned: counters.complete_records_scanned,
            retained_records_scanned: counters.retained_records_scanned,
            rejected_records_scanned: counters.rejected_records_scanned,
            ignored_records_scanned: counters.ignored_records_scanned,
        }
    }
}

#[derive(Serialize)]
struct CodexCausalDescendantReceiptV1 {
    provider_session_id: String,
    // The v1 acceptance schema predates the metadata-only opening path. Keep
    // its external field names while using precise terminology internally.
    #[serde(rename = "catalog_source_body_reads")]
    catalog_source_metadata_opens: u64,
    #[serde(rename = "catalog_source_body_bytes")]
    catalog_source_metadata_read_upper_bound_bytes: u64,
    catalog_session_meta_parses: u64,
    scanner_sources_started: u64,
    scanner_sources_completed: u64,
    scanner_bytes_read: u64,
    structural_json_parses: u64,
    typed_json_parses: u64,
    dependency_recomputations: u64,
    closure_recomputations: u64,
    lineage_fact_source_scans: u64,
    lineage_fact_source_bytes: u64,
    lineage_fact_body_bytes_read: u64,
    replaced_sources: u64,
    writer_mutated_sources: u64,
    staged_documents: u64,
    writer_exact_replay_sources: u64,
}

impl CodexCausalDescendantReceiptV1 {
    fn new(provider_session_id: String, counters: CodexSourceBackedCountersV0) -> Self {
        Self {
            provider_session_id,
            catalog_source_metadata_opens: counters.catalog_source_metadata_opens,
            catalog_source_metadata_read_upper_bound_bytes: counters
                .catalog_source_metadata_read_upper_bound_bytes,
            catalog_session_meta_parses: counters.catalog_session_meta_parses,
            scanner_sources_started: counters.scanner_sources_started,
            scanner_sources_completed: counters.scanner_sources_completed,
            scanner_bytes_read: counters.scanner_bytes_read,
            structural_json_parses: counters.structural_json_parses,
            typed_json_parses: counters.typed_json_parses,
            dependency_recomputations: 0,
            closure_recomputations: 0,
            lineage_fact_source_scans: 0,
            lineage_fact_source_bytes: 0,
            lineage_fact_body_bytes_read: 0,
            replaced_sources: counters.replaced_sources,
            writer_mutated_sources: counters.writer_mutated_sources,
            staged_documents: counters.staged_documents,
            writer_exact_replay_sources: counters.writer_exact_replay_sources,
        }
    }
}

fn current_physical_attempt_id() -> CodexSourceBackedResultV0<String> {
    let data_root = std::env::var("CTX_DATA_ROOT").map_err(|_| {
        CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(
            "Codex causal qualification cannot locate CTX_DATA_ROOT".to_owned(),
        ))
    })?;
    let job_path = Path::new(&data_root)
        .join("daemon")
        .join("jobs")
        .join("core-refresh.json");
    let job: Value = serde_json::from_slice(&fs::read(job_path)?)?;
    job.get("physical_attempt_id")
        .or_else(|| job.pointer("/structured_outcome/physical_attempt_id"))
        .or_else(|| job.get("request_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(
                "Codex causal qualification cannot resolve the physical attempt ID".to_owned(),
            ))
        })
}

fn write_atomic_json(path: &Path, receipt: &CodexCausalReceiptV1) -> CodexSourceBackedResultV0<()> {
    let parent = path.parent().ok_or_else(|| {
        CodexSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(
            "Codex causal receipt path has no parent directory".to_owned(),
        ))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}
