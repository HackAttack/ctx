use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process, thread,
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{anyhow, Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use ctx_history_core::utc_now;
use ctx_history_store::{EventEmbeddingDocument, Store};

use crate::commands::search::RefreshArg;
use crate::config::AppConfig;
use crate::output::compact_json;
use crate::SearchBackendArg;

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
const SEMANTIC_EMBED_THREADS_DEFAULT: usize = 2;
const SEMANTIC_EMBED_THREADS_MAX: usize = 8;
const SEMANTIC_EMBED_BATCH_DEFAULT: usize = 16;
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
const SEMANTIC_WORKER_QUERY_HINT_MAX_CHARS: usize = 4_096;
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
const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
const SEARCH_REFRESH_DAEMON_RECENT_MAX_AGE_MS: i64 = 120_000;
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

struct SemanticVectorHit {
    event_id: Uuid,
    similarity: f32,
    source_text_hash: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, Default)]
struct SemanticVectorSearchStats {
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

impl SemanticVectorStore {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        if !path.exists() {
            drop(
                private_create_new_file(path)
                    .with_context(|| format!("create semantic vector store {}", path.display()))?,
            );
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open semantic vector store {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let store = Self { conn };
        store.ensure_schema()?;
        secure_semantic_vector_permissions(path)?;
        Ok(store)
    }

    fn open_read_only(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open semantic vector store read-only {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_millis(SEMANTIC_VECTOR_BUSY_TIMEOUT_MS))?;
        let store = Self { conn };
        store.ensure_readable_schema()?;
        Ok(Some(store))
    }

    fn ensure_readable_schema(&self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 3 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Err(anyhow!(
                "semantic vector store is missing event_embedding_chunks"
            ));
        }
        if !sqlite_table_has_columns(
            &self.conn,
            "event_embedding_chunks",
            &[
                "event_id",
                "model_key",
                "source_text_sha256",
                "start_char",
                "end_char",
                "dimensions",
                "embedding_f32",
            ],
        )? {
            return Err(anyhow!(
                "semantic vector store event_embedding_chunks schema is incomplete"
            ));
        }
        Ok(())
    }

    fn ensure_schema(&self) -> Result<()> {
        let user_version = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        if user_version > 3 {
            return Err(anyhow!(
                "semantic vector store schema version {user_version} is newer than this ctx supports"
            ));
        }
        let mut compact_after_schema = false;
        if sqlite_table_exists(&self.conn, "event_embedding_chunks")?
            && !sqlite_table_has_columns(
                &self.conn,
                "event_embedding_chunks",
                &[
                    "event_id",
                    "model_key",
                    "history_record_id",
                    "session_id",
                    "event_seq",
                    "chunk_index",
                    "chunk_count",
                    "source_text_sha256",
                    "chunk_text_sha256",
                    "chunk_text",
                    "start_char",
                    "end_char",
                    "dimensions",
                    "embedding_f32",
                    "embedded_at_ms",
                ],
            )?
        {
            self.conn.execute("DROP TABLE event_embedding_chunks", [])?;
            compact_after_schema = true;
        }
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS embedding_models (
                model_key TEXT PRIMARY KEY,
                backend TEXT NOT NULL,
                model_id TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                distance TEXT NOT NULL,
                normalized INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_embeddings (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                text_sha256 TEXT NOT NULL,
                preview_text TEXT NOT NULL DEFAULT '',
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_seq
                ON event_embeddings(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embeddings_model_session
                ON event_embeddings(model_key, session_id);
            CREATE TABLE IF NOT EXISTS event_embedding_chunks (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                history_record_id TEXT,
                session_id TEXT,
                event_seq INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL,
                source_text_sha256 TEXT NOT NULL,
                chunk_text_sha256 TEXT NOT NULL,
                chunk_text TEXT NOT NULL DEFAULT '',
                start_char INTEGER NOT NULL,
                end_char INTEGER NOT NULL,
                dimensions INTEGER NOT NULL,
                embedding_f32 BLOB NOT NULL,
                embedded_at_ms INTEGER NOT NULL,
                PRIMARY KEY (event_id, model_key, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_seq
                ON event_embedding_chunks(model_key, event_seq);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_session
                ON event_embedding_chunks(model_key, session_id);
            CREATE INDEX IF NOT EXISTS idx_event_embedding_chunks_model_event
                ON event_embedding_chunks(model_key, event_id);
            CREATE TABLE IF NOT EXISTS semantic_index_stats (
                model_key TEXT PRIMARY KEY,
                embedded_items INTEGER NOT NULL,
                embedded_chunks INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS semantic_dirty_events (
                event_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                queued_at_ms INTEGER NOT NULL,
                priority_seq INTEGER,
                reason TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (event_id, model_key)
            );
            CREATE INDEX IF NOT EXISTS idx_semantic_dirty_events_model_priority
                ON semantic_dirty_events(model_key, priority_seq, queued_at_ms);
            PRAGMA user_version = 3;
            "#,
        )?;
        if !sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            self.conn.execute(
                "ALTER TABLE event_embeddings ADD COLUMN preview_text TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        let deleted_legacy_embeddings = self.conn.execute("DELETE FROM event_embeddings", [])?;
        let scrubbed_chunk_text = self.conn.execute(
            "UPDATE event_embedding_chunks SET chunk_text = '' WHERE chunk_text != ''",
            [],
        )?;
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO embedding_models
                (model_key, backend, model_id, dimensions, distance, normalized, created_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'cosine', 1, ?5)
            "#,
            params![
                SEMANTIC_MODEL_KEY,
                SEMANTIC_BACKEND,
                SEMANTIC_MODEL_ID,
                SEMANTIC_DIMENSIONS as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        if compact_after_schema || deleted_legacy_embeddings > 0 || scrubbed_chunk_text > 0 {
            self.compact_after_plaintext_scrub()?;
        }
        Ok(())
    }

    fn compact_after_plaintext_scrub(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA wal_checkpoint(TRUNCATE);
            VACUUM;
            "#,
        )?;
        Ok(())
    }

    fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        if !sqlite_table_exists(&self.conn, "semantic_index_stats")? {
            return Ok(None);
        }
        let stats = self
            .conn
            .query_row(
                r#"
                SELECT embedded_items, embedded_chunks
                FROM semantic_index_stats
                WHERE model_key = ?1
                "#,
                params![SEMANTIC_MODEL_KEY],
                |row| {
                    let embedded_items = row.get::<_, i64>(0)?.max(0) as usize;
                    let embedded_chunks = row.get::<_, i64>(1)?.max(0) as usize;
                    Ok(SemanticSidecarStats {
                        embedded_items,
                        embedded_chunks,
                    })
                },
            )
            .optional()?;
        Ok(stats)
    }

    fn exact_stats(&self) -> Result<SemanticSidecarStats> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticSidecarStats::default());
        }
        let embedded_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let embedded_items = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT event_id) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(SemanticSidecarStats {
            embedded_items: embedded_items.max(0) as usize,
            embedded_chunks: embedded_chunks.max(0) as usize,
        })
    }

    fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        if let Some(stats) = self.cached_stats()? {
            return Ok(stats);
        }
        self.exact_stats()
    }

    fn refresh_cached_stats(&self) -> Result<SemanticSidecarStats> {
        let stats = self.exact_stats()?;
        self.conn.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(model_key) DO UPDATE SET
                embedded_items = excluded.embedded_items,
                embedded_chunks = excluded.embedded_chunks,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                SEMANTIC_MODEL_KEY,
                stats.embedded_items as i64,
                stats.embedded_chunks as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        Ok(stats)
    }

    fn dirty_event_count(&self) -> Result<usize> {
        if !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let count = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM semantic_dirty_events WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(count.max(0) as usize)
    }

    fn enqueue_dirty_documents(
        &mut self,
        docs: &[EventEmbeddingDocument],
        reason: &str,
    ) -> Result<usize> {
        if docs.is_empty() {
            return Ok(0);
        }
        let reason = reason.chars().take(64).collect::<String>();
        let queued_at_ms = utc_now().timestamp_millis();
        let tx = self.conn.transaction()?;
        let mut changed = 0_usize;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO semantic_dirty_events
                    (event_id, model_key, queued_at_ms, priority_seq, reason, attempts)
                VALUES (?1, ?2, ?3, ?4, ?5, 0)
                ON CONFLICT(event_id, model_key) DO UPDATE SET
                    queued_at_ms = excluded.queued_at_ms,
                    priority_seq = COALESCE(excluded.priority_seq, semantic_dirty_events.priority_seq),
                    reason = excluded.reason
                "#,
            )?;
            for doc in docs {
                changed = changed.saturating_add(stmt.execute(params![
                    doc.event_id.to_string(),
                    SEMANTIC_MODEL_KEY,
                    queued_at_ms,
                    doc.seq as i64,
                    reason
                ])?);
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    fn queued_dirty_event_ids(&self, limit: usize) -> Result<Vec<Uuid>> {
        if limit == 0 || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id
            FROM semantic_dirty_events
            WHERE model_key = ?1
            ORDER BY priority_seq IS NULL, priority_seq DESC, queued_at_ms ASC
            LIMIT ?2
            "#,
        )?;
        let mut rows = stmt.query(params![SEMANTIC_MODEL_KEY, limit as i64])?;
        let mut event_ids = Vec::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            let event_id = Uuid::parse_str(&event_id_text)
                .context("invalid dirty event id in semantic vector store")?;
            event_ids.push(event_id);
        }
        Ok(event_ids)
    }

    fn dequeue_dirty_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM semantic_dirty_events WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn plaintext_value_count(&self) -> Result<usize> {
        let mut count = 0_usize;
        if sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embeddings WHERE preview_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        if sqlite_column_exists(&self.conn, "event_embedding_chunks", "chunk_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embedding_chunks WHERE chunk_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        Ok(count)
    }

    fn existing_hashes_for_event_ids(&self, event_ids: &[Uuid]) -> Result<HashMap<Uuid, String>> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(HashMap::new());
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT event_id, source_text_sha256
            FROM event_embedding_chunks
            WHERE model_key = ?
              AND event_id IN ({placeholders})
            GROUP BY event_id, source_text_sha256
            "#
        );
        let mut query_params = vec![SqlValue::from(SEMANTIC_MODEL_KEY.to_owned())];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut hashes = HashMap::new();
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            hashes.insert(event_id, row.get(1)?);
        }
        Ok(hashes)
    }

    fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut delete_stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            )?;
            let mut deleted_events = std::collections::HashSet::new();
            for (doc, _) in items {
                if deleted_events.insert(doc.event_id) {
                    delete_stmt.execute(params![doc.event_id.to_string(), SEMANTIC_MODEL_KEY])?;
                }
            }
            drop(delete_stmt);

            let mut stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, history_record_id, session_id, event_seq,
                     chunk_index, chunk_count, source_text_sha256, chunk_text_sha256,
                     chunk_text, start_char, end_char, dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
            )?;
            let embedded_at_ms = utc_now().timestamp_millis();
            for (doc, embedding) in items {
                let blob = serialize_f32_blob(embedding);
                stmt.execute(params![
                    doc.event_id.to_string(),
                    SEMANTIC_MODEL_KEY,
                    doc.history_record_id.map(|id| id.to_string()),
                    doc.session_id.map(|id| id.to_string()),
                    doc.seq as i64,
                    doc.chunk_index as i64,
                    doc.chunk_count as i64,
                    doc.source_text_hash,
                    doc.chunk_text_hash,
                    "",
                    doc.start_char as i64,
                    doc.end_char as i64,
                    SEMANTIC_DIMENSIONS as i64,
                    blob,
                    embedded_at_ms
                ])?;
            }
        }
        tx.commit()?;
        self.refresh_cached_stats()?;
        Ok(())
    }

    fn prune_ineligible_events(&mut self, store: &Store) -> Result<SemanticPruneOutcome> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticPruneOutcome::default());
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id, MIN(source_text_sha256), COUNT(DISTINCT source_text_sha256)
            FROM event_embedding_chunks
            WHERE model_key = ?1
            GROUP BY event_id
            ORDER BY MAX(event_seq) DESC
            "#,
        )?;
        let mut rows = stmt.query(params![SEMANTIC_MODEL_KEY])?;
        let mut sidecar_events = Vec::<(Uuid, String, bool)>::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            if let Ok(event_id) = Uuid::parse_str(&event_id_text) {
                let source_text_hash = row.get::<_, String>(1)?;
                let hash_versions = row.get::<_, i64>(2)?.max(0);
                sidecar_events.push((event_id, source_text_hash, hash_versions == 1));
            }
        }
        drop(rows);
        drop(stmt);

        let mut outcome = SemanticPruneOutcome::default();
        for chunk in sidecar_events.chunks(SEMANTIC_PRUNE_EVENT_BATCH) {
            let event_ids = chunk
                .iter()
                .map(|(event_id, _, _)| *event_id)
                .collect::<Vec<_>>();
            let eligible_event_ids = store.semantic_eligible_event_ids(&event_ids)?;
            let current_docs = store.event_embedding_documents_by_ids(&event_ids)?;
            let current_by_id = current_docs
                .into_iter()
                .map(|doc| (doc.event_id, doc))
                .collect::<HashMap<_, _>>();
            let mut delete_event_ids = Vec::new();
            let mut stale_docs = Vec::new();
            for (event_id, stored_hash, single_hash) in chunk {
                let Some(doc) = current_by_id.get(event_id) else {
                    delete_event_ids.push(*event_id);
                    continue;
                };
                if !eligible_event_ids.contains(event_id) {
                    delete_event_ids.push(*event_id);
                    continue;
                }
                let source_text = semantic_source_text(&doc.text);
                let current_hash = semantic_document_hash(doc, &source_text);
                if !*single_hash || current_hash != *stored_hash {
                    delete_event_ids.push(*event_id);
                    stale_docs.push(doc.clone());
                }
            }
            outcome.deleted_chunks = outcome
                .deleted_chunks
                .saturating_add(self.delete_embedding_chunks_for_event_ids(&delete_event_ids)?);
            if !stale_docs.is_empty() {
                outcome.queued_stale_events = outcome
                    .queued_stale_events
                    .saturating_add(self.enqueue_dirty_documents(&stale_docs, "stale_hash")?);
            }
        }

        let scrubbed_chunk_text = self.conn.execute(
            "UPDATE event_embedding_chunks SET chunk_text = '' WHERE model_key = ?1 AND chunk_text != ''",
            params![SEMANTIC_MODEL_KEY],
        )?;
        self.refresh_cached_stats()?;
        if scrubbed_chunk_text > 0 {
            self.compact_after_plaintext_scrub()?;
        }
        Ok(outcome)
    }

    fn delete_embedding_chunks_for_event_ids(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn search(&self, query_embedding: &[f32], limit: usize) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, None)
    }

    fn search_event_ids(
        &self,
        query_embedding: &[f32],
        event_ids: &[Uuid],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_empty() {
            return Ok(SemanticVectorSearch::default());
        }
        self.search_with_event_filter(query_embedding, limit, Some(event_ids))
    }

    fn embedded_event_id_count(&self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT COUNT(DISTINCT event_id)
            FROM event_embedding_chunks
            WHERE model_key = ?
              AND dimensions = ?
              AND event_id IN ({placeholders})
            "#
        );
        let mut query_params = vec![
            SqlValue::from(SEMANTIC_MODEL_KEY.to_owned()),
            SqlValue::from(SEMANTIC_DIMENSIONS as i64),
        ];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let count = self
            .conn
            .query_row(&sql, params_from_iter(query_params), |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(count.max(0) as usize)
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
        event_ids: Option<&[Uuid]>,
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticVectorSearch {
                hits: Vec::new(),
                stats: SemanticVectorSearchStats {
                    scan_ms: scan_started.elapsed().as_millis() as u64,
                    ..SemanticVectorSearchStats::default()
                },
            });
        }
        let mut sql = r#"
            SELECT event_id, source_text_sha256, start_char, end_char, embedding_f32
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND dimensions = ?2
            "#
        .to_owned();
        let mut query_params = vec![
            SqlValue::from(SEMANTIC_MODEL_KEY.to_owned()),
            SqlValue::from(SEMANTIC_DIMENSIONS as i64),
        ];
        if let Some(event_ids) = event_ids {
            let placeholders = (0..event_ids.len())
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(" AND event_id IN (");
            sql.push_str(&placeholders);
            sql.push(')');
            query_params.extend(
                event_ids
                    .iter()
                    .map(|event_id| SqlValue::from(event_id.to_string())),
            );
        } else {
            sql.push_str(" ORDER BY event_seq DESC LIMIT ?");
            query_params.push(SqlValue::from(SEMANTIC_FULL_SCAN_MAX_CHUNKS as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let limit = limit.max(1);
        let mut chunks_scanned = 0_usize;
        let mut vector_bytes_read = 0_usize;
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            let source_text_hash = row.get::<_, String>(1)?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let blob: Vec<u8> = row.get(4)?;
            chunks_scanned = chunks_scanned.saturating_add(1);
            vector_bytes_read = vector_bytes_read.saturating_add(blob.len());
            if event_ids.is_none() && vector_bytes_read > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES {
                break;
            }
            let Some(similarity) = dot_product_f32_blob(query_embedding, &blob)? else {
                continue;
            };
            match best_by_event.get_mut(&event_id) {
                Some(existing) if similarity > existing.similarity => {
                    *existing = SemanticVectorHit {
                        event_id,
                        similarity,
                        source_text_hash,
                        start_char,
                        end_char,
                    };
                }
                None => {
                    best_by_event.insert(
                        event_id,
                        SemanticVectorHit {
                            event_id,
                            similarity,
                            source_text_hash,
                            start_char,
                            end_char,
                        },
                    );
                }
                _ => {}
            }
        }
        let events_scored = best_by_event.len();
        let mut top = best_by_event.into_values().collect::<Vec<_>>();
        if top.len() > limit {
            top.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
            top.truncate(limit);
        }
        top.sort_by(compare_semantic_hits_desc);
        Ok(SemanticVectorSearch {
            hits: top,
            stats: SemanticVectorSearchStats {
                scan_ms: scan_started.elapsed().as_millis() as u64,
                chunks_scanned,
                vector_bytes_read,
                events_scored,
            },
        })
    }
}

fn semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join("vectors.sqlite")
}

fn semantic_worker_lock_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_LOCK_FILE)
}

fn semantic_worker_status_path(data_root: &Path) -> PathBuf {
    data_root.join(SEMANTIC_WORKER_STATUS_FILE)
}

fn daemon_root_path(data_root: &Path) -> PathBuf {
    data_root.join(DAEMON_DIR)
}

fn daemon_jobs_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_JOBS_DIR)
}

fn daemon_lock_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_LOCK_FILE)
}

fn daemon_status_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_STATUS_FILE)
}

fn daemon_history_refresh_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_HISTORY_REFRESH_JOB_FILE)
}

fn daemon_semantic_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_SEMANTIC_JOB_FILE)
}

fn daemon_cloud_sync_job_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(DAEMON_CLOUD_SYNC_JOB_FILE)
}

struct DaemonLock {
    path: PathBuf,
}

impl DaemonLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let root = daemon_root_path(data_root);
        create_private_dir_all(&root)?;
        let path = daemon_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                        "binary": env::current_exe().ok(),
                        "data_root": data_root,
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && daemon_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("create ctx daemon lock {}", path.display()));
                }
            }
        }
        Ok(None)
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct SemanticWorkerLock {
    path: PathBuf,
}

impl SemanticWorkerLock {
    fn acquire(data_root: &Path) -> Result<Option<Self>> {
        create_private_dir_all(data_root)?;
        let path = semantic_worker_lock_path(data_root);
        for attempt in 0..2 {
            match private_create_new_file(&path) {
                Ok(mut file) => {
                    let payload = json!({
                        "pid": process::id(),
                        "started_at_ms": utc_now().timestamp_millis(),
                    });
                    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && semantic_worker_lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Ok(None);
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("create semantic worker lock {}", path.display())
                    });
                }
            }
        }
        Ok(None)
    }
}

impl Drop for SemanticWorkerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn semantic_worker_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn daemon_lock_is_stale(path: &Path) -> bool {
    pid_lock_file_is_stale(path)
}

fn pid_lock_file_is_stale(path: &Path) -> bool {
    let Some(value) = read_pid_lock_json(path) else {
        return path.exists();
    };
    if lock_started_at_is_stale(&value) {
        return true;
    }
    let Some(pid) = pid_from_lock_json(&value) else {
        return true;
    };
    !pid_is_running(pid)
}

fn read_pid_lock_file(path: &Path) -> Option<u32> {
    read_pid_lock_json(path).and_then(|value| pid_from_lock_json(&value))
}

fn read_pid_lock_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn pid_from_lock_json(value: &Value) -> Option<u32> {
    value
        .get("pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

fn lock_started_at_is_stale(value: &Value) -> bool {
    let Some(started_at_ms) = json_i64(value, "started_at_ms") else {
        return false;
    };
    utc_now().timestamp_millis().saturating_sub(started_at_ms) > DAEMON_LOCK_STALE_AFTER_MS
}

#[cfg(unix)]
fn pid_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_running(pid: u32) -> bool {
    pid != 0
}

#[cfg(unix)]
fn lower_semantic_worker_priority() {
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(not(unix))]
fn lower_semantic_worker_priority() {}

fn write_private_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("json.{}.tmp", process::id()));
    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }
    let mut file = private_create_new_file(&tmp_path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write private status file {}", tmp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync private status file {}", tmp_path.display()))?;
    drop(file);
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("replace private status file {}", path.display()))?;
    secure_private_file_permissions(&path)?;
    Ok(())
}

fn write_semantic_worker_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&semantic_worker_status_path(data_root), value)
}

fn read_semantic_worker_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(semantic_worker_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_status(data_root: &Path, value: &Value) -> Result<()> {
    write_private_json_file(&daemon_status_path(data_root), value)
}

fn read_daemon_status(data_root: &Path) -> Option<Value> {
    let text = fs::read_to_string(daemon_status_path(data_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    write_private_json_file(path, value)
}

fn read_daemon_job_status(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn semantic_status_file_stats(status_value: Option<&Value>) -> SemanticSidecarStats {
    SemanticSidecarStats {
        embedded_items: status_value
            .and_then(|value| json_usize(value, "embedded_items"))
            .unwrap_or(0),
        embedded_chunks: status_value
            .and_then(|value| json_usize(value, "embedded_chunks"))
            .unwrap_or(0),
    }
}

pub(crate) fn semantic_worker_report(
    data_root: &Path,
    store: Option<&Store>,
) -> Result<SemanticWorkerReport> {
    let status_value = read_semantic_worker_status(data_root);
    let searchable_items = match store {
        Some(store) => store.event_embedding_document_count_cached_or_exact()?,
        None => status_value
            .as_ref()
            .and_then(|value| json_usize(value, "searchable_items"))
            .unwrap_or(0),
    };
    let vector_path = semantic_vector_path(data_root);
    let model_cache_available =
        semantic_model_cache_available(&semantic_worker_cache_dir(data_root));
    let sidecar_state_result = (|| -> Result<(SemanticSidecarStats, usize)> {
        if let Some(vector_store) = SemanticVectorStore::open_read_only(&vector_path)? {
            let dirty_items = vector_store.dirty_event_count()?;
            let mut stats = vector_store.cached_or_exact_stats()?;
            if semantic_status_needs_exact_sidecar_stats(searchable_items, dirty_items, stats) {
                stats = vector_store.exact_stats()?;
            }
            Ok((stats, dirty_items))
        } else if store.is_some() {
            Ok((SemanticSidecarStats::default(), 0))
        } else {
            Ok((semantic_status_file_stats(status_value.as_ref()), 0))
        }
    })();
    let (sidecar_stats, dirty_items, sidecar_error) = match sidecar_state_result {
        Ok((stats, dirty_items)) => (stats, dirty_items, None),
        Err(error) => (
            SemanticSidecarStats {
                embedded_items: 0,
                embedded_chunks: 0,
            },
            0,
            Some(format!("{error:#}")),
        ),
    };
    let embedded_items = sidecar_stats.embedded_items;
    let embedded_chunks = sidecar_stats.embedded_chunks;
    let status_path = semantic_worker_status_path(data_root);
    let lock_path = semantic_worker_lock_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let running = lock_pid.is_some_and(pid_is_running);
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    let queued_items_estimate = searchable_items
        .saturating_sub(embedded_items)
        .max(dirty_items);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| {
            if store.is_none() {
                "unknown".to_owned()
            } else if searchable_items == 0 {
                "empty".to_owned()
            } else if queued_items_estimate == 0 {
                "ready".to_owned()
            } else {
                "pending".to_owned()
            }
        });
    if store.is_some() {
        let live_status = if searchable_items == 0 {
            "empty".to_owned()
        } else if sidecar_error.is_some() {
            "unavailable".to_owned()
        } else if queued_items_estimate == 0 {
            "ready".to_owned()
        } else {
            "pending".to_owned()
        };
        status = if status == "budget_exhausted" && queued_items_estimate > 0 {
            status
        } else if status == "failed"
            && sidecar_error.is_none()
            && embedded_items == 0
            && queued_items_estimate > 0
        {
            status
        } else {
            live_status
        };
    }
    if running {
        status = "running".to_owned();
    } else if lock_path.exists() && semantic_worker_lock_is_stale(&lock_path) {
        status = "stale_lock".to_owned();
    }
    Ok(SemanticWorkerReport {
        status,
        running,
        pid,
        started_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "started_at_ms")),
        heartbeat_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "heartbeat_at_ms")),
        finished_at_ms: status_value
            .as_ref()
            .and_then(|value| json_i64(value, "finished_at_ms")),
        indexed_chunks: status_value
            .as_ref()
            .and_then(|value| json_usize(value, "indexed_chunks")),
        model_init_ms: status_value
            .as_ref()
            .and_then(|value| json_usize(value, "model_init_ms")),
        last_error: sidecar_error.or_else(|| {
            status_value
                .as_ref()
                .and_then(|value| json_string(value, "last_error"))
        }),
        searchable_items,
        embedded_items,
        embedded_chunks,
        dirty_items,
        queued_items_estimate,
        model_cache_available,
        vector_path,
        lock_path,
        status_path,
    })
}

pub(crate) fn semantic_worker_report_best_effort(data_root: &Path) -> SemanticWorkerReport {
    semantic_worker_report(data_root, None)
        .unwrap_or_else(|error| SemanticWorkerReport::unavailable(data_root, format!("{error:#}")))
}

pub(crate) fn daemon_report(data_root: &Path, semantic_report: &SemanticWorkerReport) -> Value {
    daemon_report_with_disabled_status(data_root, semantic_report, true)
}

fn daemon_report_with_disabled_status(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_status(data_root);
    let lock_path = daemon_lock_path(data_root);
    let status_path = daemon_status_path(data_root);
    let lock_pid = read_pid_lock_file(&lock_path);
    let running = lock_pid.is_some_and(pid_is_running);
    let mut status = status_value
        .as_ref()
        .and_then(|value| json_string(value, "status"))
        .unwrap_or_else(|| "unknown".to_owned());
    if running {
        status = "running".to_owned();
    } else if lock_path.exists() && daemon_lock_is_stale(&lock_path) {
        status = "stale_lock".to_owned();
    } else if !enabled && (disabled_overrides_lifecycle || status == "unknown") {
        status = "disabled".to_owned();
    }
    let pid = if running {
        lock_pid
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_u32(value, "pid"))
    };
    compact_json(json!({
        "status": status,
        "enabled": enabled,
        "running": running,
        "pid": pid,
        "started_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "started_at_ms")),
        "heartbeat_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "heartbeat_at_ms")),
        "finished_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "finished_at_ms")),
        "last_error": status_value.as_ref().and_then(|value| json_string(value, "last_error")),
        "lock_path": lock_path,
        "status_path": status_path,
        "jobs": {
            "history_refresh": daemon_history_refresh_job_report(
                data_root,
                disabled_overrides_lifecycle
            ),
            "semantic_index": daemon_semantic_job_report(
                data_root,
                semantic_report,
                disabled_overrides_lifecycle
            ),
            "cloud_sync": daemon_cloud_sync_job_report(data_root),
        },
    }))
}

fn daemon_history_refresh_job_report(
    data_root: &Path,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_history_refresh_job_path(data_root));
    let disabled = !daemon_enabled && disabled_overrides_lifecycle;
    let current_status = if disabled {
        "disabled".to_owned()
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "status"))
            .unwrap_or_else(|| "unknown".to_owned())
    };
    let reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else {
        status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason"))
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": reason,
        "mode": status_value
            .as_ref()
            .and_then(|value| json_string(value, "mode"))
            .unwrap_or_else(|| RefreshArg::Auto.as_str().to_owned()),
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "source_count": status_value.as_ref().and_then(|value| value.get("source_count").cloned()),
        "source_fingerprint": status_value
            .as_ref()
            .and_then(|value| json_string(value, "source_fingerprint")),
        "passes": status_value.as_ref().and_then(|value| json_usize(value, "passes")),
        "totals": status_value.as_ref().and_then(|value| value.get("totals").cloned()),
        "budget_reasons": status_value
            .as_ref()
            .and_then(|value| value.get("budget_reasons").cloned()),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error")),
    }))
}

fn daemon_enabled_for_status(data_root: &Path) -> bool {
    AppConfig::load(data_root)
        .map(|config| config.daemon.enabled)
        .unwrap_or_else(|_| AppConfig::default().daemon.enabled)
}

fn daemon_semantic_job_report(
    data_root: &Path,
    semantic_report: &SemanticWorkerReport,
    disabled_overrides_lifecycle: bool,
) -> Value {
    let daemon_enabled = daemon_enabled_for_status(data_root);
    let status_value = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    let disabled = !daemon_enabled && disabled_overrides_lifecycle && !semantic_report.running;
    let current_status = if disabled {
        "disabled"
    } else if semantic_report.running {
        "running"
    } else if semantic_report.status == "stale_lock" {
        "stale_lock"
    } else if semantic_report.status == "unavailable" {
        "unavailable"
    } else if semantic_report.searchable_items == 0 {
        "empty"
    } else if semantic_report.queued_items_estimate == 0 {
        "ready"
    } else if !semantic_report.model_cache_available {
        "skipped"
    } else if semantic_report.status == "failed" {
        "failed"
    } else {
        "pending"
    };
    let derived_reason = if disabled {
        Some("daemon_disabled".to_owned())
    } else if semantic_report.status == "stale_lock" {
        Some("worker_lock_stale".to_owned())
    } else if semantic_report.status == "unavailable" {
        Some("sidecar_unavailable".to_owned())
    } else if semantic_report.searchable_items == 0 {
        Some("no_searchable_items".to_owned())
    } else if semantic_report.queued_items_estimate > 0 && !semantic_report.model_cache_available {
        Some("model_cache_missing".to_owned())
    } else if semantic_report.status == "failed" {
        Some("worker_failed".to_owned())
    } else {
        None
    };
    compact_json(json!({
        "status": current_status,
        "enabled": daemon_enabled,
        "reason": derived_reason,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_run_status": status_value
            .as_ref()
            .and_then(|value| json_string(value, "status")),
        "last_run_reason": status_value
            .as_ref()
            .and_then(|value| json_string(value, "reason")),
        "last_error": status_value
            .as_ref()
            .and_then(|value| json_string(value, "last_error"))
            .or_else(|| semantic_report.last_error.clone()),
        "indexed_chunks": status_value.as_ref().and_then(|value| json_usize(value, "indexed_chunks")),
        "model_cache_available": semantic_report.model_cache_available,
        "worker_status": semantic_report.status,
        "coverage": {
            "searchable_items": semantic_report.searchable_items,
            "completed_items": semantic_report.embedded_items,
            "embedded_items": semantic_report.embedded_items,
            "embedded_chunks": semantic_report.embedded_chunks,
            "dirty_items": semantic_report.dirty_items,
            "queued_items_estimate": semantic_report.queued_items_estimate,
        },
    }))
}

fn daemon_cloud_sync_job_report(data_root: &Path) -> Value {
    let status_value = read_daemon_job_status(&daemon_cloud_sync_job_path(data_root));
    compact_json(json!({
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": status_value.as_ref().and_then(|value| json_i64(value, "last_run_at_ms")),
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    secure_private_dir_permissions(path)?;
    Ok(())
}

fn private_create_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

#[cfg(unix)]
fn secure_private_dir_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_private_file_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_semantic_vector_permissions(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("secure semantic vector file {}", candidate.display()))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_semantic_vector_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn sqlite_column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_table_has_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut existing = std::collections::HashSet::new();
    while let Some(row) = rows.next()? {
        existing.insert(row.get::<_, String>(1)?);
    }
    Ok(columns.iter().all(|column| existing.contains(*column)))
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn semantic_query_text(query: &str, terms: &[String]) -> String {
    let mut parts = Vec::new();
    if !query.trim().is_empty() {
        parts.push(query.trim().to_owned());
    }
    parts.extend(
        terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(str::to_owned),
    );
    parts.join(" ")
}

fn semantic_filters_need_overfetch(filters: &ctx_history_search::SearchFilters) -> bool {
    semantic_filters_require_lexical_fallback(filters)
        || !filters.include_subagents
        || filters.exclude_provider_session.is_some()
}

fn semantic_filters_require_lexical_fallback(filters: &ctx_history_search::SearchFilters) -> bool {
    filters.session.is_some()
        || filters.provider.is_some()
        || filters.history_source.is_some()
        || filters.provider_key.is_some()
        || filters.source_id.is_some()
        || filters.source_format.is_some()
        || filters
            .repo
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.since.is_some()
        || filters.primary_only
        || filters.event_type.is_some()
        || filters
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn semantic_hybrid_coverage_ready(embedded_items: usize, searchable_items: usize) -> bool {
    if embedded_items == 0 {
        return false;
    }
    if searchable_items == 0 {
        return true;
    }
    embedded_items >= SEMANTIC_HYBRID_MIN_EMBEDDED_ITEMS
        || (embedded_items as f64 / searchable_items as f64) >= SEMANTIC_HYBRID_MIN_COVERAGE_RATIO
}

fn semantic_status_needs_exact_sidecar_stats(
    searchable_items: usize,
    dirty_items: usize,
    stats: SemanticSidecarStats,
) -> bool {
    if searchable_items == 0 || dirty_items > 0 {
        return false;
    }
    stats.embedded_items >= searchable_items
        || !semantic_hybrid_coverage_ready(stats.embedded_items, searchable_items)
}

fn semantic_auto_candidate_event_ids_from_packet(
    packet: &ctx_history_search::SearchPacket,
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    let mut event_ids = Vec::new();
    for result in &packet.results {
        if let Some(event_id) = result.event_id {
            if seen.insert(event_id) {
                event_ids.push(event_id);
            }
        }
    }
    event_ids
}

fn semantic_auto_candidate_coverage_ready(
    embedded_candidates: usize,
    total_candidates: usize,
) -> bool {
    total_candidates > 0 && embedded_candidates == total_candidates
}

fn reciprocal_rank(rank: usize) -> f32 {
    1.0 / (60.0 + rank.max(1) as f32)
}

fn push_unique_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|value| value == reason) {
        reasons.push(reason.to_owned());
    }
}

fn normalize_packet_result_ranks(results: &mut [ctx_history_search::SearchPacketResult]) {
    let max_rank = results
        .iter()
        .map(|result| result.rank)
        .fold(0.0_f32, f32::max);
    if max_rank <= 0.0 {
        return;
    }
    for result in results {
        result.rank = (result.rank / max_rank).clamp(0.0, 1.0);
        if result.result_scope == ctx_history_search::SearchResultScope::Session {
            result.session_importance =
                session_importance(result.rank, result.more_matches_in_session);
        } else {
            result.session_importance = 0.0;
        }
    }
}

fn session_importance(rank: f32, more_matches_in_session: usize) -> f32 {
    let coverage_boost = ((more_matches_in_session as f32).ln_1p() * 0.08).min(0.24);
    (rank + coverage_boost).clamp(0.0, 1.0)
}

fn compare_packet_results(
    left: &ctx_history_search::SearchPacketResult,
    right: &ctx_history_search::SearchPacketResult,
) -> std::cmp::Ordering {
    right
        .rank
        .partial_cmp(&left.rank)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| right.timestamp.cmp(&left.timestamp))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

fn semantic_auto_rerank_packet(
    mut packet: ctx_history_search::SearchPacket,
    semantic_hits: &[ctx_history_search::SemanticEventHit],
    semantic_weight: f32,
) -> ctx_history_search::SearchPacket {
    let semantic_weight = semantic_weight.clamp(0.0, 1.0);
    let mut semantic_by_event = HashMap::<Uuid, f32>::new();
    let mut semantic_by_session = HashMap::<Uuid, f32>::new();
    for (index, semantic_hit) in semantic_hits.iter().enumerate() {
        let score = reciprocal_rank(index + 1);
        semantic_by_event
            .entry(semantic_hit.hit.event_id)
            .and_modify(|existing| *existing = existing.max(score))
            .or_insert(score);
        if let Some(session_id) = semantic_hit.hit.session_id {
            semantic_by_session
                .entry(session_id)
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
        }
    }

    for (index, result) in packet.results.iter_mut().enumerate() {
        let lexical = reciprocal_rank(index + 1);
        let semantic = result
            .event_id
            .and_then(|event_id| semantic_by_event.get(&event_id).copied())
            .or_else(|| {
                result
                    .session_id
                    .and_then(|session_id| semantic_by_session.get(&session_id).copied())
            })
            .unwrap_or(0.0);
        result.rank = ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic);
        if semantic > 0.0 {
            push_unique_reason(&mut result.why_matched, "semantic_similarity");
            push_unique_reason(&mut result.why_matched, "semantic:auto_rerank");
        }
    }
    packet.results.sort_by(compare_packet_results);
    normalize_packet_result_ranks(&mut packet.results);
    packet
}

fn semantic_hits_for_text_query(
    store: &Store,
    vector_store: &SemanticVectorStore,
    cache_dir: &Path,
    semantic_text: &str,
    limit: usize,
    event_filter: Option<&[Uuid]>,
) -> Result<(
    Vec<ctx_history_search::SemanticEventHit>,
    SemanticRetrievalDiagnostics,
)> {
    let query_embed_started = Instant::now();
    let mut embedder = new_semantic_embedder(cache_dir)?;
    let mut embeddings = embed_texts(&mut embedder, vec![semantic_text.to_owned()])?;
    let query_embed_ms = query_embed_started.elapsed().as_millis() as u64;
    let query_embedding = embeddings
        .pop()
        .ok_or_else(|| anyhow!("semantic query embedding was empty"))?;
    let semantic_hit_search =
        semantic_hits_for_query(store, vector_store, &query_embedding, limit, event_filter)?;
    let mut diagnostics = semantic_hit_search.diagnostics;
    diagnostics.query_embed_ms = Some(query_embed_ms);
    Ok((semantic_hit_search.hits, diagnostics))
}

struct SemanticEmbedder {
    model: TextEmbedding,
    batch_size: usize,
}

fn new_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    let options = TextInitOptions::new(EmbeddingModel::AllMiniLML6V2)
        .with_show_download_progress(false)
        .with_intra_threads(semantic_embedder_threads())
        .with_cache_dir(cache_dir.to_path_buf());
    let previous_hf_home = env::var_os("HF_HOME");
    env::set_var("HF_HOME", cache_dir);
    let model_result = TextEmbedding::try_new(options);
    if let Some(previous_hf_home) = previous_hf_home {
        env::set_var("HF_HOME", previous_hf_home);
    } else {
        env::remove_var("HF_HOME");
    }
    let model = model_result
        .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))?;
    Ok(SemanticEmbedder {
        model,
        batch_size: semantic_embed_batch_size(),
    })
}

fn semantic_embedder_threads() -> usize {
    env_usize("CTX_SEMANTIC_THREADS")
        .map(|value| value.min(SEMANTIC_EMBED_THREADS_MAX))
        .or_else(|| {
            thread::available_parallelism()
                .ok()
                .map(|threads| threads.get().min(SEMANTIC_EMBED_THREADS_DEFAULT).max(1))
        })
        .unwrap_or(SEMANTIC_EMBED_THREADS_DEFAULT)
}

fn semantic_embed_batch_size() -> usize {
    env_usize("CTX_SEMANTIC_EMBED_BATCH")
        .map(|value| value.min(SEMANTIC_EMBED_BATCH_MAX))
        .unwrap_or(SEMANTIC_EMBED_BATCH_DEFAULT)
}

fn semantic_cache_dir() -> Option<PathBuf> {
    env::var("CTX_SEMANTIC_CACHE_DIR")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn semantic_worker_cache_dir(data_root: &Path) -> PathBuf {
    env::var("HF_HOME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(semantic_cache_dir)
        .or_else(|| {
            env::var("FASTEMBED_CACHE_DIR")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| data_root.join("semantic-model-cache"))
}

fn semantic_model_cache_available(cache_dir: &Path) -> bool {
    if cache_dir.as_os_str().is_empty() {
        return false;
    }
    let model_root = cache_dir.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let Ok(snapshot_ref) = fs::read_to_string(model_root.join("refs").join("main")) else {
        return false;
    };
    let snapshot_ref = snapshot_ref.trim();
    if snapshot_ref.is_empty()
        || snapshot_ref.contains('/')
        || snapshot_ref.contains('\\')
        || snapshot_ref == "."
        || snapshot_ref == ".."
    {
        return false;
    }
    let snapshot = model_root.join("snapshots").join(snapshot_ref);
    if !snapshot.is_dir() {
        return false;
    }
    SEMANTIC_REQUIRED_MODEL_FILES.iter().all(|file| {
        fs::metadata(snapshot.join(file))
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
    })
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn embed_texts(embedder: &mut SemanticEmbedder, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let mut embeddings = embedder
        .model
        .embed(texts, Some(embedder.batch_size))
        .with_context(|| format!("embed text with semantic model {SEMANTIC_MODEL_ID}"))?;
    for embedding in &mut embeddings {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "semantic model returned {} dimensions, expected {}",
                embedding.len(),
                SEMANTIC_DIMENSIONS
            ));
        }
        normalize_embedding(embedding);
    }
    Ok(embeddings)
}

fn backfill_semantic_embeddings(
    store: &Store,
    vector_store: &mut SemanticVectorStore,
    embedder: &mut Option<SemanticEmbedder>,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    query_text: Option<&str>,
    max_to_index: usize,
    json_output: bool,
    continue_past_indexed_pages: bool,
    deadline: Option<Instant>,
) -> Result<usize> {
    let mut existing_hashes = HashMap::new();
    let mut before = None;
    let mut indexed = 0_usize;
    let mut scanned = 0_usize;

    let dirty_ids =
        vector_store.queued_dirty_event_ids(max_to_index.min(SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT))?;
    if !dirty_ids.is_empty() && indexed < max_to_index {
        let docs = store.event_embedding_documents_by_ids(&dirty_ids)?;
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        let found_event_ids = docs.iter().map(|doc| doc.event_id).collect::<HashSet<_>>();
        let mut consumed_event_ids = dirty_ids
            .iter()
            .filter(|event_id| !found_event_ids.contains(event_id))
            .copied()
            .collect::<Vec<_>>();
        scanned = scanned.saturating_add(docs.len());
        let outcome = index_semantic_documents(
            vector_store,
            embedder,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        indexed = indexed.saturating_add(outcome.indexed_chunks);
        consumed_event_ids.extend(outcome.consumed_event_ids);
        if !consumed_event_ids.is_empty() {
            vector_store.dequeue_dirty_events(&consumed_event_ids)?;
        }
        if indexed > 0 && !json_output {
            eprintln!(
                "semantic index: embedded {indexed} dirty-priority chunks (scanned {scanned} events)"
            );
        }
    }

    if indexed < max_to_index {
        if let Some(query_text) = query_text {
            let terms = semantic_backfill_terms(query_text);
            if !terms.is_empty() {
                let remaining = max_to_index.saturating_sub(indexed);
                let docs = store.event_embedding_documents_matching_terms(&terms, remaining)?;
                extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
                scanned = scanned.saturating_add(docs.len());
                let outcome = index_semantic_documents(
                    vector_store,
                    embedder,
                    model_init_ms,
                    cache_dir,
                    &mut existing_hashes,
                    docs,
                    remaining,
                    deadline,
                )?;
                indexed = indexed.saturating_add(outcome.indexed_chunks);
                if outcome.indexed_chunks > 0 && !json_output {
                    eprintln!(
                        "semantic index: embedded {indexed} query-directed chunks (scanned {scanned} events)"
                    );
                }
            }
        }
    }

    while indexed < max_to_index {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let docs = store.recent_event_embedding_documents(before, 512)?;
        if docs.is_empty() {
            break;
        }
        before = docs.last().map(|doc| (doc.occurred_at_ms, doc.seq));
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        scanned = scanned.saturating_add(docs.len());
        let outcome = index_semantic_documents(
            vector_store,
            embedder,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        let added = outcome.indexed_chunks;
        indexed = indexed.saturating_add(added);
        if !json_output {
            eprintln!("semantic index: embedded {indexed} chunks (scanned {scanned} events)");
        }
        if added == 0 && !continue_past_indexed_pages {
            break;
        }
    }
    Ok(indexed)
}

fn extend_existing_hashes_for_docs(
    vector_store: &SemanticVectorStore,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: &[EventEmbeddingDocument],
) -> Result<()> {
    let event_ids = docs
        .iter()
        .map(|doc| doc.event_id)
        .filter(|event_id| !existing_hashes.contains_key(event_id))
        .collect::<Vec<_>>();
    if event_ids.is_empty() {
        return Ok(());
    }
    existing_hashes.extend(vector_store.existing_hashes_for_event_ids(&event_ids)?);
    Ok(())
}

fn index_semantic_documents(
    vector_store: &mut SemanticVectorStore,
    embedder: &mut Option<SemanticEmbedder>,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: Vec<EventEmbeddingDocument>,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<SemanticIndexOutcome> {
    let limit = semantic_deadline_chunk_limit(limit, deadline);
    if limit == 0 {
        return Ok(SemanticIndexOutcome::default());
    }
    let mut pending = Vec::<SemanticChunkDocument>::new();
    let mut unchanged_event_ids = Vec::new();
    let mut pending_event_ids = Vec::new();
    for doc in docs {
        let source_text = semantic_source_text(&doc.text);
        let text_hash = semantic_document_hash(&doc, &source_text);
        if existing_hashes
            .get(&doc.event_id)
            .is_some_and(|existing| existing == &text_hash)
        {
            unchanged_event_ids.push(doc.event_id);
            continue;
        }
        let chunks = semantic_chunks_for_document(&doc, &source_text, &text_hash);
        if chunks.len() > limit && pending.is_empty() {
            continue;
        }
        if pending.len().saturating_add(chunks.len()) > limit && !pending.is_empty() {
            break;
        }
        pending_event_ids.push(doc.event_id);
        pending.extend(chunks);
        if pending.len() >= limit {
            break;
        }
    }
    if pending.is_empty() {
        return Ok(SemanticIndexOutcome {
            indexed_chunks: 0,
            consumed_event_ids: unchanged_event_ids,
        });
    }
    let texts = pending
        .iter()
        .map(|doc| doc.text.clone())
        .collect::<Vec<_>>();
    if embedder.is_none() {
        if !semantic_deadline_has_model_init_budget(deadline) {
            return Ok(SemanticIndexOutcome {
                indexed_chunks: 0,
                consumed_event_ids: unchanged_event_ids,
            });
        }
        let model_init_started = Instant::now();
        *embedder = Some(new_semantic_embedder(cache_dir)?);
        *model_init_ms = Some(model_init_started.elapsed().as_millis() as u64);
    }
    let embedder = embedder
        .as_mut()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?;
    let embeddings = embed_texts(embedder, texts)?;
    let items = pending
        .into_iter()
        .zip(embeddings.into_iter())
        .map(|(doc, embedding)| {
            existing_hashes.insert(doc.event_id, doc.source_text_hash.clone());
            (doc, embedding)
        })
        .collect::<Vec<_>>();
    vector_store.upsert_chunk_embeddings(&items)?;
    unchanged_event_ids.extend(pending_event_ids);
    Ok(SemanticIndexOutcome {
        indexed_chunks: items.len(),
        consumed_event_ids: unchanged_event_ids,
    })
}

fn semantic_deadline_chunk_limit(limit: usize, deadline: Option<Instant>) -> usize {
    let Some(deadline) = deadline else {
        return limit;
    };
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return 0;
    };
    let seconds = remaining.as_secs() as usize;
    if seconds == 0 {
        return 0;
    }
    let deadline_limit = seconds
        .saturating_mul(SEMANTIC_DEADLINE_CHUNKS_PER_SECOND)
        .max(SEMANTIC_DEADLINE_MIN_CHUNK_BATCH);
    limit.min(deadline_limit)
}

fn semantic_deadline_has_model_init_budget(deadline: Option<Instant>) -> bool {
    let Some(deadline) = deadline else {
        return true;
    };
    deadline
        .checked_duration_since(Instant::now())
        .is_some_and(|remaining| {
            remaining >= StdDuration::from_secs(SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS)
        })
}

fn semantic_source_text(text: &str) -> String {
    text.chars().take(SEMANTIC_SOURCE_MAX_CHARS).collect()
}

fn semantic_chunks_for_document(
    doc: &EventEmbeddingDocument,
    source_text: &str,
    source_text_hash: &str,
) -> Vec<SemanticChunkDocument> {
    let chunks = semantic_text_chunks(source_text);
    let chunk_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(
            |(chunk_index, (start_char, end_char, text))| SemanticChunkDocument {
                event_id: doc.event_id,
                history_record_id: doc.history_record_id,
                session_id: doc.session_id,
                seq: doc.seq,
                chunk_index,
                chunk_count,
                source_text_hash: source_text_hash.to_owned(),
                chunk_text_hash: semantic_text_hash(&semantic_embedded_chunk_text(doc, &text)),
                text: semantic_embedded_chunk_text(doc, &text),
                start_char,
                end_char,
            },
        )
        .collect()
}

fn semantic_document_hash(doc: &EventEmbeddingDocument, source_text: &str) -> String {
    semantic_text_hash(&semantic_embedded_document_text(doc, source_text))
}

fn semantic_embedded_document_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    semantic_embedded_chunk_text(doc, body)
}

fn semantic_embedded_chunk_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    let header = semantic_document_header(doc);
    if header.is_empty() {
        body.to_owned()
    } else {
        format!("{header}\n\n{body}")
    }
}

fn semantic_document_header(doc: &EventEmbeddingDocument) -> String {
    let mut lines = vec![
        "semantic_document: v2".to_owned(),
        format!("event_type: {}", doc.event_type.as_str()),
    ];
    if let Some(role) = doc.role {
        lines.push(format!("role: {}", role.as_str()));
    }
    if !doc.rank_bucket.trim().is_empty() {
        lines.push(format!(
            "rank_bucket: {}",
            semantic_header_value(&doc.rank_bucket, 80)
        ));
    }
    if let Some(provider) = doc.provider {
        lines.push(format!("provider: {}", provider.as_str()));
    }
    if let Some(source_format) = doc.source_format.as_deref() {
        lines.push(format!(
            "source_format: {}",
            semantic_header_value(source_format, 120)
        ));
    }
    if let Some(agent_type) = doc.agent_type {
        lines.push(format!("agent_type: {}", agent_type.as_str()));
    }
    if let Some(is_primary) = doc.session_is_primary {
        lines.push(format!(
            "session_scope: {}",
            if is_primary { "primary" } else { "subagent" }
        ));
    }
    if let Some(workspace) = doc.record_workspace.as_deref() {
        lines.push(format!(
            "workspace_hint: {}",
            semantic_header_value(workspace, 160)
        ));
    }
    if let Some(cwd) = doc.cwd.as_deref().and_then(path_basename) {
        lines.push(format!("cwd_hint: {}", semantic_header_value(cwd, 120)));
    }
    if let Some(path) = doc.raw_source_path.as_deref().and_then(path_basename) {
        lines.push(format!(
            "source_file_hint: {}",
            semantic_header_value(path, 120)
        ));
    }
    if let Some(title) = doc.record_title.as_deref() {
        lines.push(format!("title_hint: {}", semantic_header_value(title, 180)));
    }
    if let Some(kind) = doc.record_kind.as_deref() {
        lines.push(format!("record_kind: {}", semantic_header_value(kind, 80)));
    }
    lines.join("\n")
}

fn semantic_header_value(value: &str, max_chars: usize) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = sanitized.chars().take(max_chars).collect::<String>();
    if sanitized.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn path_basename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|value| value.to_str())
}

fn semantic_text_chunks(text: &str) -> Vec<(usize, usize, String)> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= SEMANTIC_CHUNK_TARGET_CHARS {
        return vec![(0, chars.len(), text.to_owned())];
    }

    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < chars.len() {
        let mut end = start
            .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
            .min(chars.len());
        if end < chars.len() {
            let boundary_floor = end.saturating_sub(150).max(start + 1);
            for index in (boundary_floor..end).rev() {
                if chars[index].is_whitespace() {
                    end = index + 1;
                    break;
                }
            }
        }
        if end <= start {
            end = start
                .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
                .min(chars.len());
        }
        let chunk = chars[start..end].iter().collect::<String>();
        chunks.push((start, end, chunk));
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(SEMANTIC_CHUNK_OVERLAP_CHARS);
    }
    chunks
}

fn semantic_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize_embedding(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in values {
            *value /= norm;
        }
    }
}

fn semantic_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_lowercase();
            if token.len() < 2 {
                None
            } else {
                Some(stem_semantic_token(&token))
            }
        })
        .collect()
}

fn semantic_backfill_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::<String>::new();
    for token in semantic_tokens(text) {
        push_unique_term(&mut terms, &token);
        match canonical_semantic_token(&token) {
            Some("email") => {
                for term in ["mail", "email", "inbox", "mailbox", "zoho", "smtp"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("send_limit") => {
                for term in ["throttle", "limit", "blocked", "bulk", "send", "sending"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("agent_memory") => {
                for term in ["agentmemory", "memory", "memories"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("outreach") => {
                for term in ["outreach", "lead", "enrich", "campaign", "reply"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("hosted_team") => {
                for term in ["hosted", "cloud", "enterprise", "team", "shared"] {
                    push_unique_term(&mut terms, term);
                }
            }
            Some("market") => {
                for term in ["competitor", "pricing", "price", "matrix"] {
                    push_unique_term(&mut terms, term);
                }
            }
            _ => {}
        }
    }
    terms.truncate(20);
    terms
}

fn push_unique_term(terms: &mut Vec<String>, term: &str) {
    if term.len() >= 3 && !terms.iter().any(|existing| existing == term) {
        terms.push(term.to_owned());
    }
}

fn stem_semantic_token(token: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if token.len() > suffix.len() + 3 && token.ends_with(suffix) {
            return token[..token.len() - suffix.len()].to_owned();
        }
    }
    token.to_owned()
}

fn canonical_semantic_token(token: &str) -> Option<&'static str> {
    match token {
        "mail" | "email" | "inbox" | "mailbox" | "mx" | "spf" | "dmarc" | "smtp" | "zoho" => {
            Some("email")
        }
        "throttle" | "limit" | "quota" | "blocked" | "bulk" | "spike" | "send" | "sender"
        | "sending" => Some("send_limit"),
        "admin" | "reauth" | "password" | "delete" | "auth" => Some("auth_admin"),
        "agentmemory" | "memory" | "memories" | "remember" => Some("agent_memory"),
        "outreach" | "lead" | "leads" | "enrich" | "campaign" | "reply" | "buyer" => {
            Some("outreach")
        }
        "hosted" | "cloud" | "enterprise" | "team" | "shared" => Some("hosted_team"),
        "competitor" | "competitors" | "pricing" | "price" | "matrix" => Some("market"),
        "privacy" | "private" | "scoped" | "scope" | "governance" | "policy" => Some("governance"),
        "semantic" | "hybrid" | "vector" | "embedding" | "embeddings" => Some("semantic"),
        "subagent" | "subagents" | "worker" | "workers" => Some("subagent"),
        _ => None,
    }
}

fn serialize_f32_blob(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(values.len() * 4);
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

fn dot_product_f32_blob(left: &[f32], right_blob: &[u8]) -> Result<Option<f32>> {
    if right_blob.len() % 4 != 0 {
        return Err(anyhow!(
            "invalid semantic vector blob length {}",
            right_blob.len()
        ));
    }
    if right_blob.len() / 4 != left.len() {
        return Ok(None);
    }
    let mut sum = 0.0_f32;
    for (value, chunk) in left.iter().zip(right_blob.chunks_exact(4)) {
        sum += value * f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(Some(sum))
}

fn compare_semantic_hits_desc(
    left: &SemanticVectorHit,
    right: &SemanticVectorHit,
) -> std::cmp::Ordering {
    right
        .similarity
        .partial_cmp(&left.similarity)
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn semantic_hits_for_query(
    store: &Store,
    vector_store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
    event_filter: Option<&[Uuid]>,
) -> Result<SemanticHitSearch> {
    let vector_limit = limit.saturating_mul(SEMANTIC_VECTOR_OVERFETCH).max(limit);
    let vector_search = if let Some(event_filter) = event_filter {
        vector_store.search_event_ids(query_embedding, event_filter, vector_limit)?
    } else {
        vector_store.search(query_embedding, vector_limit)?
    };
    let mut diagnostics = SemanticRetrievalDiagnostics {
        vector_scan_ms: Some(vector_search.stats.scan_ms),
        chunks_scanned: Some(vector_search.stats.chunks_scanned),
        vector_bytes_read: Some(vector_search.stats.vector_bytes_read),
        events_scored: Some(vector_search.stats.events_scored),
        ..SemanticRetrievalDiagnostics::default()
    };
    let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
    for hit in vector_search.hits {
        let replace = best_by_event
            .get(&hit.event_id)
            .map(|existing| hit.similarity > existing.similarity)
            .unwrap_or(true);
        if replace {
            best_by_event.insert(hit.event_id, hit);
        }
    }
    let mut vector_hits = best_by_event.into_values().collect::<Vec<_>>();
    vector_hits.sort_by(compare_semantic_hits_desc);
    let current_hashes = current_semantic_source_hashes(store, &vector_hits)?;
    let before_stale_filter = vector_hits.len();
    vector_hits.retain(|hit| {
        current_hashes
            .get(&hit.event_id)
            .is_some_and(|hash| hash == &hit.source_text_hash)
    });
    if vector_hits.len() > limit {
        vector_hits.truncate(limit);
    }
    diagnostics.stale_events_dropped = Some(before_stale_filter.saturating_sub(vector_hits.len()));
    let chunk_ranges = vector_hits
        .iter()
        .map(|hit| (hit.event_id, (hit.start_char, hit.end_char)))
        .collect::<HashMap<_, _>>();
    let hydration_started = Instant::now();
    let hydrated_hits = store.semantic_event_hits_by_id(&chunk_ranges)?;
    diagnostics.hydration_ms = Some(hydration_started.elapsed().as_millis() as u64);
    let hydrated_by_id = hydrated_hits
        .into_iter()
        .map(|hit| (hit.event_id, hit))
        .collect::<HashMap<_, _>>();
    let mut hits = Vec::new();
    for vector_hit in vector_hits {
        if let Some(hit) = hydrated_by_id.get(&vector_hit.event_id).cloned() {
            hits.push(ctx_history_search::SemanticEventHit {
                hit,
                similarity: vector_hit.similarity,
            });
        }
    }
    diagnostics.semantic_candidates = Some(hits.len());
    Ok(SemanticHitSearch { hits, diagnostics })
}

fn current_semantic_source_hashes(
    store: &Store,
    vector_hits: &[SemanticVectorHit],
) -> Result<HashMap<Uuid, String>> {
    let event_ids = vector_hits
        .iter()
        .map(|hit| hit.event_id)
        .collect::<Vec<_>>();
    let docs = store.event_embedding_documents_by_ids(&event_ids)?;
    Ok(docs
        .into_iter()
        .map(|doc| {
            let source_text = semantic_source_text(&doc.text);
            (doc.event_id, semantic_document_hash(&doc, &source_text))
        })
        .collect())
}
