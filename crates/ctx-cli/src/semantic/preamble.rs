use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(ctx_sqlite_vec)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Once,
};

use anyhow::{anyhow, Context, Result};
#[cfg(ctx_semantic_fastembed)]
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ctx_history_core::{database_path, utc_now};
use ctx_history_store::{EventEmbeddingDocument, Store};

use crate::commands::{
    import::{error_summary, import_totals_json, ImportTotals},
    search::{refresh_sources_for_search, search_refresh_sources, RefreshArg},
};
use crate::config::{self, AppConfig, CONFIG_FILE};
use crate::output::{compact_json, print_json};
use crate::store_util::{open_existing_store_read_only, open_existing_store_snapshot_read_only};
use crate::{
    DaemonArgs, DaemonCommand, DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg,
    JsonArgs, SearchBackendArg,
};

const SEMANTIC_BACKEND: &str = "fastembed";
const SEMANTIC_MODEL_KEY: &str = "fastembed:all-MiniLM-L6-v2:semantic-payload-chunk-1200-200-v2";
const SEMANTIC_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";
const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--Qdrant--all-MiniLM-L6-v2-onnx";
const SEMANTIC_REQUIRED_MODEL_FILES: &[&str] = &[
    "model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];
const SEMANTIC_DIMENSIONS: usize = 384;
const SEMANTIC_SEARCH_CANDIDATES: usize = 200;
const SEMANTIC_FILTERED_SEARCH_CANDIDATES: usize = 1_000;
const SEMANTIC_CHUNK_TARGET_CHARS: usize = 1_200;
pub(crate) const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = 200;
const SEMANTIC_SOURCE_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_VECTOR_OVERFETCH: usize = 4;
const SEMANTIC_FULL_SCAN_MAX_CHUNKS: usize = 250_000;
const SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES: usize = 512 * 1024 * 1024;
const SEMANTIC_VECTOR_BACKEND_RUST: &str = "rust_blob_scan";
const SEMANTIC_VECTOR_BACKEND_SQLITE_VEC: &str = "sqlite_vec0";
const SEMANTIC_SQLITE_VEC0_MAX_K: usize = 4_096;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_THREADS_DEFAULT: usize = 2;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_THREADS_MAX: usize = 8;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_BATCH_DEFAULT: usize = 16;
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_EMBED_BATCH_MAX: usize = 512;
const SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT: usize = 512;
const SEMANTIC_WORKER_LOCK_FILE: &str = "semantic-worker.lock";
const SEMANTIC_WORKER_STATUS_FILE: &str = "semantic-worker.json";
const SEMANTIC_WORKER_BATCH_DEFAULT: usize = 128;
pub(crate) const SEMANTIC_WORKER_BATCH_MAX: usize = 5_000;
const SEMANTIC_WORKER_MAX_SECONDS_DEFAULT: u64 = 60;
pub(crate) const SEMANTIC_WORKER_MAX_SECONDS_CAP: u64 = 3_600;
const SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS: u64 = 15;
const SEMANTIC_VECTOR_BUSY_TIMEOUT_MS: u64 = 30_000;
const SEMANTIC_PRUNE_EVENT_BATCH: usize = 1_000;
const SEMANTIC_DEADLINE_CHUNKS_PER_SECOND: usize = 3;
const SEMANTIC_DEADLINE_MIN_CHUNK_BATCH: usize = 16;
const DAEMON_DIR: &str = "daemon";
const DAEMON_JOBS_DIR: &str = "jobs";
const DAEMON_LOCK_FILE: &str = "daemon.lock";
const DAEMON_STATUS_FILE: &str = "status.json";
const DAEMON_HISTORY_REFRESH_JOB_FILE: &str = "history-refresh.json";
const DAEMON_SEMANTIC_JOB_FILE: &str = "semantic-index.json";
const DAEMON_CLOUD_SYNC_JOB_FILE: &str = "cloud-sync.json";
const DAEMON_MAX_RUNTIME_SECONDS_DEFAULT: u64 = 300;
const DAEMON_IDLE_EXIT_SECONDS_DEFAULT: u64 = 30;
const DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
pub(crate) const DAEMON_RUNTIME_SECONDS_CAP: u64 = 24 * 60 * 60;
const DAEMON_AUTOSTART_MAX_RUNTIME_SECONDS_DEFAULT: u64 = 45;
const DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT: u64 = 5;
const DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";
const DAEMON_AUTOSTART_OFF_ENV: &str = "CTX_DAEMON_AUTOSTART_OFF";
const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
const DAEMON_SEMANTIC_RESERVE_GRACE_SECS: u64 = 10;
const DAEMON_MIN_REMAINING_FOR_JOB_SECS: u64 = 2;
const SEMANTIC_HYBRID_MIN_EMBEDDED_ITEMS: usize = 1_000;
const SEMANTIC_HYBRID_MIN_COVERAGE_RATIO: f64 = 0.01;

#[derive(Debug, Clone)]
pub(crate) struct SemanticWorkerReport {
    status: String,
    running: bool,
    pid: Option<u32>,
    started_at_ms: Option<i64>,
    heartbeat_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    indexed_chunks: Option<usize>,
    model_init_ms: Option<usize>,
    last_error: Option<String>,
    searchable_items: usize,
    embedded_items: usize,
    embedded_chunks: usize,
    dirty_items: usize,
    queued_items_estimate: usize,
    model_cache_available: bool,
    vector_path: PathBuf,
    lock_path: PathBuf,
    status_path: PathBuf,
}

impl SemanticWorkerReport {
    fn unavailable(data_root: &Path, error: impl ToString) -> Self {
        Self {
            status: "unavailable".to_owned(),
            running: false,
            pid: None,
            started_at_ms: None,
            heartbeat_at_ms: None,
            finished_at_ms: None,
            indexed_chunks: None,
            model_init_ms: None,
            last_error: Some(error.to_string()),
            searchable_items: 0,
            embedded_items: 0,
            embedded_chunks: 0,
            dirty_items: 0,
            queued_items_estimate: 0,
            model_cache_available: semantic_model_cache_available(&semantic_worker_cache_dir(
                data_root,
            )),
            vector_path: semantic_vector_path(data_root),
            lock_path: semantic_worker_lock_path(data_root),
            status_path: semantic_worker_status_path(data_root),
        }
    }

    fn coverage_ratio(&self) -> Option<f64> {
        if self.searchable_items == 0 {
            None
        } else {
            Some((self.embedded_items as f64 / self.searchable_items as f64).min(1.0))
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "status": self.status,
            "running": self.running,
            "pid": self.pid,
            "started_at_ms": self.started_at_ms,
            "heartbeat_at_ms": self.heartbeat_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "indexed_chunks": self.indexed_chunks,
            "model_init_ms": self.model_init_ms,
            "last_error": self.last_error,
            "coverage": {
                "searchable_items": self.searchable_items,
                "embedded_items": self.embedded_items,
                "embedded_chunks": self.embedded_chunks,
                "dirty_items": self.dirty_items,
                "queued_items_estimate": self.queued_items_estimate,
                "coverage_ratio": self.coverage_ratio(),
            },
            "model_cache_available": self.model_cache_available,
            "vector_path": self.vector_path.display().to_string(),
            "lock_path": self.lock_path.display().to_string(),
            "status_path": self.status_path.display().to_string(),
        }))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticRetrievalReport {
    requested_mode: SearchBackendArg,
    effective_mode: SearchBackendArg,
    semantic_weight: f32,
    semantic_status: &'static str,
    semantic_fallback_code: Option<&'static str>,
    semantic_fallback: Option<String>,
    embedding_model: Option<String>,
    embedded_items: usize,
    embedded_chunks: usize,
    searchable_items: usize,
    indexed_now: usize,
    vector_path: Option<PathBuf>,
    worker: Option<SemanticWorkerReport>,
    diagnostics: Option<SemanticRetrievalDiagnostics>,
}

impl SemanticRetrievalReport {
    pub(crate) fn lexical(requested_mode: SearchBackendArg, searchable_items: usize) -> Self {
        Self {
            requested_mode,
            effective_mode: SearchBackendArg::Lexical,
            semantic_weight: 0.0,
            semantic_status: "skipped",
            semantic_fallback_code: None,
            semantic_fallback: None,
            embedding_model: None,
            embedded_items: 0,
            embedded_chunks: 0,
            searchable_items,
            indexed_now: 0,
            vector_path: None,
            worker: None,
            diagnostics: None,
        }
    }

    fn apply_worker_counts(&mut self, worker: &SemanticWorkerReport) {
        self.searchable_items = worker.searchable_items;
        self.embedded_items = worker.embedded_items;
        self.embedded_chunks = worker.embedded_chunks;
    }

    fn apply_worker_coverage(&mut self, worker: &SemanticWorkerReport) {
        self.apply_worker_counts(worker);
        self.semantic_status = semantic_status_from_worker(worker);
    }

    fn set_semantic_fallback(&mut self, code: &'static str, message: impl Into<String>) {
        self.semantic_fallback_code = Some(code);
        self.semantic_fallback = Some(message.into());
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "requested_mode": self.requested_mode.as_str(),
            "effective_mode": self.effective_mode.as_str(),
            "semantic_weight": self.semantic_weight,
            "semantic_status": self.semantic_status,
            "semantic_fallback_code": self.semantic_fallback_code,
            "semantic_fallback": self.semantic_fallback,
            "embedding_model": self.embedding_model,
            "coverage": {
                "embedded_items": self.embedded_items,
                "embedded_chunks": self.embedded_chunks,
                "searchable_items": self.searchable_items,
                "indexed_now": self.indexed_now,
                "dirty_items": self.worker.as_ref().map(|worker| worker.dirty_items),
            },
            "vector_path": self.vector_path.as_ref().map(|path| path.display().to_string()),
            "worker": self.worker.as_ref().map(SemanticWorkerReport::to_json),
            "diagnostics": self.diagnostics.as_ref().map(SemanticRetrievalDiagnostics::to_json),
        }))
    }

    pub(crate) fn effective_mode(&self) -> SearchBackendArg {
        self.effective_mode
    }
}

fn semantic_status_from_worker(worker: &SemanticWorkerReport) -> &'static str {
    if worker.searchable_items == 0 || worker.embedded_items == 0 {
        "unavailable"
    } else if semantic_worker_coverage_ready(worker) {
        "ready"
    } else {
        "partial"
    }
}

fn semantic_worker_coverage_ready(worker: &SemanticWorkerReport) -> bool {
    worker.searchable_items > 0
        && worker.embedded_items >= worker.searchable_items
        && worker.dirty_items == 0
}

#[derive(Debug, Clone, Default)]
struct SemanticRetrievalDiagnostics {
    vector_backend: Option<&'static str>,
    query_embed_ms: Option<u64>,
    vector_scan_ms: Option<u64>,
    chunks_scanned: Option<usize>,
    vector_bytes_read: Option<usize>,
    events_scored: Option<usize>,
    hydration_ms: Option<u64>,
    stale_events_dropped: Option<usize>,
    semantic_candidates: Option<usize>,
    auto_candidate_count: Option<usize>,
    auto_embedded_candidate_count: Option<usize>,
    auto_hybrid_skipped: Option<&'static str>,
}

impl SemanticRetrievalDiagnostics {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "vector_backend": self.vector_backend,
            "query_embed_ms": self.query_embed_ms,
            "vector_scan_ms": self.vector_scan_ms,
            "chunks_scanned": self.chunks_scanned,
            "vector_bytes_read": self.vector_bytes_read,
            "events_scored": self.events_scored,
            "hydration_ms": self.hydration_ms,
            "stale_events_dropped": self.stale_events_dropped,
            "semantic_candidates": self.semantic_candidates,
            "auto_candidate_count": self.auto_candidate_count,
            "auto_embedded_candidate_count": self.auto_embedded_candidate_count,
            "auto_hybrid_skipped": self.auto_hybrid_skipped,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_packet_with_backend(
    store: &Store,
    data_root: &Path,
    query: &str,
    terms: &[String],
    options: &ctx_history_search::PacketOptions,
    requested_backend: SearchBackendArg,
    semantic_weight: f32,
    refresh_mode: RefreshArg,
    emit_warnings: bool,
) -> Result<(ctx_history_search::SearchPacket, SemanticRetrievalReport)> {
    let uses_composed_terms = terms.iter().any(|term| !term.trim().is_empty());
    let semantic_text = semantic_query_text(query, terms);
    let semantic_cache_dir = semantic_worker_cache_dir(data_root);
    let vector_path = semantic_vector_path(data_root);
    let mut effective_backend = requested_backend;

    let filters_require_semantic_fallback =
        matches!(
            effective_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        ) && semantic_filters_require_lexical_fallback(&options.filters);
    let terms_require_semantic_fallback = matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && uses_composed_terms;
    if filters_require_semantic_fallback || terms_require_semantic_fallback {
        effective_backend = SearchBackendArg::Lexical;
    }

    let worker_report = if requested_backend == SearchBackendArg::Auto
        || matches!(
            effective_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        ) {
        semantic_worker_report(data_root, Some(store))?
    } else {
        semantic_worker_report_best_effort(data_root)
    };
    let searchable_items = worker_report.searchable_items;
    let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, searchable_items);
    retrieval.worker = Some(worker_report.clone());
    retrieval.apply_worker_counts(&worker_report);
    if matches!(
        requested_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        retrieval.apply_worker_coverage(&worker_report);
    }

    if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && semantic_text.trim().is_empty()
    {
        return Err(anyhow!(
            "semantic search needs a text query; add a query or --term"
        ));
    }

    if filters_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "filtered_vector_lookup_unsupported",
            "semantic search does not yet support filtered vector lookup",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet support these filters; falling back to lexical search",
        );
    } else if terms_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "term_or_semantics_unsupported",
            "semantic search does not yet preserve --term OR semantics",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet preserve --term OR semantics; falling back to lexical search",
        );
    }

    let lexical_search_packet = || -> Result<ctx_history_search::SearchPacket> {
        if uses_composed_terms {
            ctx_history_search::search_packet_terms(store, query, terms, options)
                .map_err(Into::into)
        } else {
            ctx_history_search::search_packet(store, query, options).map_err(Into::into)
        }
    };

    let packet = if requested_backend == SearchBackendArg::Auto {
        auto_search_packet(
            store,
            options,
            &lexical_search_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &semantic_cache_dir,
            &semantic_text,
            semantic_weight,
            refresh_mode,
        )?
    } else if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_or_hybrid_search_packet(
            store,
            options,
            &lexical_search_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &semantic_cache_dir,
            &semantic_text,
            effective_backend,
            semantic_weight,
            emit_warnings,
        )?
    } else {
        lexical_search_packet()?
    };

    Ok((packet, retrieval))
}

#[allow(clippy::too_many_arguments)]
fn auto_search_packet(
    store: &Store,
    options: &ctx_history_search::PacketOptions,
    lexical_search_packet: &dyn Fn() -> Result<ctx_history_search::SearchPacket>,
    retrieval: &mut SemanticRetrievalReport,
    worker_report: &SemanticWorkerReport,
    vector_path: &Path,
    semantic_cache_dir: &Path,
    semantic_text: &str,
    semantic_weight: f32,
    _refresh_mode: RefreshArg,
) -> Result<ctx_history_search::SearchPacket> {
    let lexical_packet = lexical_search_packet()?;
    let mut auto_diagnostics = SemanticRetrievalDiagnostics::default();
    let auto_skip_reason = if semantic_text.trim().is_empty() {
        Some("empty_semantic_query")
    } else if semantic_filters_require_lexical_fallback(&options.filters) {
        Some("unsupported_filter_or_terms")
    } else {
        None
    };
    if let Some(reason) = auto_skip_reason {
        auto_diagnostics.auto_hybrid_skipped = Some(reason);
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    let auto_candidate_ids = semantic_auto_candidate_event_ids_from_packet(&lexical_packet);
    auto_diagnostics.auto_candidate_count = Some(auto_candidate_ids.len());
    if auto_candidate_ids.len() == 1 {
        auto_diagnostics.auto_hybrid_skipped = Some("candidate_count_too_small");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    let vector_store = match SemanticVectorStore::open_read_only(vector_path) {
        Ok(Some(vector_store)) => vector_store,
        Ok(None) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_index_unavailable");
            retrieval.diagnostics = Some(auto_diagnostics);
            return Ok(lexical_packet);
        }
        Err(_) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_index_open_error");
            retrieval.semantic_status = "unavailable";
            retrieval.diagnostics = Some(auto_diagnostics);
            return Ok(lexical_packet);
        }
    };

    if auto_candidate_ids.is_empty() && !semantic_worker_coverage_ready(worker_report) {
        auto_diagnostics.auto_hybrid_skipped = Some("semantic_coverage_not_ready");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    if !worker_report.model_cache_available || !semantic_model_cache_available(semantic_cache_dir) {
        auto_diagnostics.auto_hybrid_skipped = Some("model_cache_missing");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    if auto_candidate_ids.is_empty() {
        *retrieval = SemanticRetrievalReport {
            requested_mode: SearchBackendArg::Auto,
            effective_mode: SearchBackendArg::Semantic,
            semantic_weight: 1.0,
            semantic_status: semantic_status_from_worker(worker_report),
            semantic_fallback_code: None,
            semantic_fallback: None,
            embedding_model: Some(SEMANTIC_MODEL_ID.to_owned()),
            embedded_items: worker_report.embedded_items,
            embedded_chunks: worker_report.embedded_chunks,
            searchable_items: worker_report.searchable_items,
            indexed_now: 0,
            vector_path: Some(vector_path.to_path_buf()),
            worker: Some(worker_report.clone()),
            diagnostics: None,
        };
        let semantic_candidate_limit =
            SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8));
        return match semantic_hits_for_text_query(
            store,
            &vector_store,
            semantic_cache_dir,
            semantic_text,
            semantic_candidate_limit,
            None,
        ) {
            Ok((semantic_hits, mut diagnostics)) if !semantic_hits.is_empty() => {
                diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
                retrieval.diagnostics = Some(diagnostics);
                ctx_history_search::semantic_event_search_packet(
                    store,
                    semantic_text,
                    options,
                    &semantic_hits,
                    1.0,
                    false,
                )
                .map_err(Into::into)
            }
            Ok((_semantic_hits, mut diagnostics)) => {
                diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
                diagnostics.auto_hybrid_skipped = Some("no_semantic_candidates");
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.vector_path = None;
                retrieval.diagnostics = Some(diagnostics);
                Ok(lexical_packet)
            }
            Err(_) => {
                auto_diagnostics.auto_hybrid_skipped = Some("semantic_retrieval_failed");
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.semantic_status = "unavailable";
                retrieval.diagnostics = Some(auto_diagnostics);
                Ok(lexical_packet)
            }
        };
    }

    let embedded_candidate_count = vector_store.embedded_event_id_count(&auto_candidate_ids)?;
    auto_diagnostics.auto_embedded_candidate_count = Some(embedded_candidate_count);
    if !semantic_auto_candidate_coverage_ready(embedded_candidate_count, auto_candidate_ids.len()) {
        auto_diagnostics.auto_hybrid_skipped = Some("candidate_coverage_not_ready");
        retrieval.diagnostics = Some(auto_diagnostics);
        return Ok(lexical_packet);
    }

    *retrieval = SemanticRetrievalReport {
        requested_mode: SearchBackendArg::Auto,
        effective_mode: SearchBackendArg::Hybrid,
        semantic_weight,
        semantic_status: semantic_status_from_worker(worker_report),
        semantic_fallback_code: None,
        semantic_fallback: None,
        embedding_model: Some(SEMANTIC_MODEL_ID.to_owned()),
        embedded_items: worker_report.embedded_items,
        embedded_chunks: worker_report.embedded_chunks,
        searchable_items: worker_report.searchable_items,
        indexed_now: 0,
        vector_path: Some(vector_path.to_path_buf()),
        worker: Some(worker_report.clone()),
        diagnostics: None,
    };
    match semantic_hits_for_text_query(
        store,
        &vector_store,
        semantic_cache_dir,
        semantic_text,
        auto_candidate_ids.len().max(options.limit),
        Some(&auto_candidate_ids),
    ) {
        Ok((semantic_hits, mut diagnostics)) if !semantic_hits.is_empty() => {
            diagnostics.auto_candidate_count = auto_diagnostics.auto_candidate_count;
            diagnostics.auto_embedded_candidate_count =
                auto_diagnostics.auto_embedded_candidate_count;
            retrieval.diagnostics = Some(diagnostics);
            Ok(semantic_auto_rerank_packet(
                lexical_packet,
                &semantic_hits,
                semantic_weight,
            ))
        }
        Ok((_semantic_hits, mut diagnostics)) => {
            diagnostics.auto_hybrid_skipped = Some("no_semantic_candidates");
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.diagnostics = Some(diagnostics);
            Ok(lexical_packet)
        }
        Err(_) => {
            auto_diagnostics.auto_hybrid_skipped = Some("semantic_retrieval_failed");
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.semantic_status = "unavailable";
            retrieval.diagnostics = Some(auto_diagnostics);
            Ok(lexical_packet)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn semantic_or_hybrid_search_packet(
    store: &Store,
    options: &ctx_history_search::PacketOptions,
    lexical_search_packet: &dyn Fn() -> Result<ctx_history_search::SearchPacket>,
    retrieval: &mut SemanticRetrievalReport,
    worker_report: &SemanticWorkerReport,
    vector_path: &Path,
    semantic_cache_dir: &Path,
    semantic_text: &str,
    effective_backend: SearchBackendArg,
    semantic_weight: f32,
    emit_warnings: bool,
) -> Result<ctx_history_search::SearchPacket> {
    match SemanticVectorStore::open_read_only(vector_path) {
        Ok(Some(vector_store)) => {
            *retrieval = SemanticRetrievalReport {
                requested_mode: retrieval.requested_mode,
                effective_mode: effective_backend,
                semantic_weight: if effective_backend == SearchBackendArg::Hybrid {
                    semantic_weight
                } else {
                    1.0
                },
                semantic_status: semantic_status_from_worker(worker_report),
                semantic_fallback_code: None,
                semantic_fallback: None,
                embedding_model: Some(SEMANTIC_MODEL_ID.to_owned()),
                embedded_items: worker_report.embedded_items,
                embedded_chunks: worker_report.embedded_chunks,
                searchable_items: worker_report.searchable_items,
                indexed_now: 0,
                vector_path: Some(vector_path.to_path_buf()),
                worker: Some(worker_report.clone()),
                diagnostics: None,
            };

            if worker_report.embedded_items == 0 {
                if effective_backend == SearchBackendArg::Semantic {
                    if !worker_report.model_cache_available
                        || !semantic_model_cache_available(semantic_cache_dir)
                    {
                        return Err(anyhow!(
                            "semantic index has no embedded event chunks and semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                        ));
                    }
                    return Err(anyhow!(
                        "semantic index has no embedded event chunks yet; ctx search does not start semantic indexing"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "semantic_index_empty",
                    "semantic index has no embedded event chunks",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic index is empty; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if effective_backend == SearchBackendArg::Hybrid
                && !semantic_hybrid_coverage_ready(
                    worker_report.embedded_items,
                    worker_report.searchable_items,
                )
            {
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "semantic_coverage_not_ready",
                    format!(
                        "semantic coverage is too low for hybrid ranking ({}/{} events embedded)",
                        worker_report.embedded_items, worker_report.searchable_items
                    ),
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic coverage is too low for hybrid ranking; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if !worker_report.model_cache_available
                || !semantic_model_cache_available(semantic_cache_dir)
            {
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(anyhow!(
                        "semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "model_cache_missing",
                    "semantic model is not available in the local cache",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic model is not available in the local cache; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            let semantic_candidate_limit = if semantic_filters_need_overfetch(&options.filters) {
                SEMANTIC_FILTERED_SEARCH_CANDIDATES.max(options.limit.saturating_mul(100))
            } else {
                SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8))
            };
            match semantic_hits_for_text_query(
                store,
                &vector_store,
                semantic_cache_dir,
                semantic_text,
                semantic_candidate_limit,
                None,
            ) {
                Ok((semantic_hits, diagnostics)) => {
                    retrieval.diagnostics = Some(diagnostics);
                    ctx_history_search::semantic_event_search_packet(
                        store,
                        semantic_text,
                        options,
                        &semantic_hits,
                        semantic_weight,
                        effective_backend == SearchBackendArg::Hybrid,
                    )
                    .map_err(Into::into)
                }
                Err(error) => {
                    if effective_backend == SearchBackendArg::Semantic {
                        return Err(anyhow!("semantic search failed: {error:#}"));
                    }
                    retrieval.effective_mode = SearchBackendArg::Lexical;
                    retrieval.semantic_weight = 0.0;
                    retrieval.embedding_model = None;
                    retrieval.semantic_status = "unavailable";
                    retrieval.diagnostics = None;
                    retrieval.set_semantic_fallback(
                        "semantic_retrieval_failed",
                        format!("semantic retrieval failed: {error:#}"),
                    );
                    warn_if(
                        emit_warnings,
                        "warning: semantic retrieval failed; falling back to lexical search",
                    );
                    lexical_search_packet()
                }
            }
        }
        Ok(None) => {
            if effective_backend == SearchBackendArg::Semantic {
                if !worker_report.model_cache_available
                    || !semantic_model_cache_available(semantic_cache_dir)
                {
                    return Err(anyhow!(
                        "semantic index is not available yet and semantic model is not available in the local cache; strict semantic search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                return Err(anyhow!(
                    "semantic index is not available yet; ctx search does not start semantic indexing"
                ));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.set_semantic_fallback(
                "semantic_index_missing",
                "semantic index is not available yet",
            );
            warn_if(
                emit_warnings,
                "warning: semantic index is not available yet; falling back to lexical search",
            );
            lexical_search_packet()
        }
        Err(error) => {
            let message = format!("semantic index could not be opened: {error:#}");
            if effective_backend == SearchBackendArg::Semantic {
                return Err(anyhow!(message));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.semantic_status = "unavailable";
            retrieval.set_semantic_fallback("semantic_index_open_error", message);
            warn_if(
                emit_warnings,
                "warning: semantic index could not be opened; falling back to lexical search",
            );
            lexical_search_packet()
        }
    }
}

fn warn_if(enabled: bool, message: &str) {
    if enabled {
        eprintln!("{message}");
    }
}

#[cfg(ctx_sqlite_vec)]
fn register_sqlite_vec_auto_extension() -> bool {
    static REGISTER: Once = Once::new();
    static AVAILABLE: AtomicBool = AtomicBool::new(false);

    REGISTER.call_once(|| {
        let rc = unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const ()
            )))
        };
        AVAILABLE.store(rc == rusqlite::ffi::SQLITE_OK, Ordering::Relaxed);
    });

    AVAILABLE.load(Ordering::Relaxed)
}

#[cfg(not(ctx_sqlite_vec))]
fn register_sqlite_vec_auto_extension() -> bool {
    false
}

struct SemanticVectorHit {
    event_id: Uuid,
    similarity: f32,
    source_text_hash: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, Default)]
struct SemanticVectorSearchStats {
    backend: Option<&'static str>,
    scan_ms: u64,
    chunks_scanned: usize,
    vector_bytes_read: usize,
    events_scored: usize,
}

#[derive(Default)]
struct SemanticVectorSearch {
    hits: Vec<SemanticVectorHit>,
    stats: SemanticVectorSearchStats,
}

struct SemanticHitSearch {
    hits: Vec<ctx_history_search::SemanticEventHit>,
    diagnostics: SemanticRetrievalDiagnostics,
}

#[derive(Debug, Clone)]
struct SemanticChunkDocument {
    event_id: Uuid,
    history_record_id: Option<Uuid>,
    session_id: Option<Uuid>,
    seq: u64,
    chunk_index: usize,
    chunk_count: usize,
    source_text_hash: String,
    chunk_text_hash: String,
    text: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct SemanticSidecarStats {
    embedded_items: usize,
    embedded_chunks: usize,
}

#[derive(Debug, Default)]
struct SemanticIndexOutcome {
    indexed_chunks: usize,
    consumed_event_ids: Vec<Uuid>,
}

#[derive(Debug, Default)]
struct SemanticPruneOutcome {
    deleted_chunks: usize,
    queued_stale_events: usize,
}

struct SemanticVectorStore {
    conn: Connection,
}
