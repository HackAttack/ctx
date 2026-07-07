fn semantic_env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(crate) fn semantic_health_findings(data_root: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    let semantic_lock = semantic_worker_lock_path(data_root);
    if semantic_lock.exists() && semantic_worker_lock_is_stale(&semantic_lock) {
        findings.push(format!(
            "semantic worker lock is stale: {}",
            semantic_lock.display()
        ));
    }
    let daemon_lock = daemon_lock_path(data_root);
    if daemon_lock.exists() && daemon_lock_is_stale(&daemon_lock) {
        findings.push(format!("daemon lock is stale: {}", daemon_lock.display()));
    }
    if let Some(status) = read_semantic_worker_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("semantic worker last failed: {error}"));
        }
    }
    if let Some(status) = read_daemon_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("daemon last failed: {error}"));
        }
    }
    let vector_path = semantic_vector_path(data_root);
    if vector_path.exists() {
        match SemanticVectorStore::open_read_only(&vector_path) {
            Ok(Some(vector_store)) => match vector_store.plaintext_value_count() {
                Ok(0) => {}
                Ok(count) => findings.push(format!(
                    "semantic vector sidecar contains {count} plaintext value(s); run daemon maintenance to scrub it"
                )),
                Err(error) => findings.push(format!(
                    "semantic vector sidecar plaintext check failed: {error:#}"
                )),
            },
            Ok(None) => {}
            Err(error) => findings.push(format!(
                "semantic vector sidecar is unreadable at {}: {error:#}",
                vector_path.display()
            )),
        }
    }
    findings
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

fn sqlite_table_sql(conn: &Connection, table: &str) -> Result<Option<String>> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(sql)
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

#[cfg(ctx_semantic_fastembed)]
struct SemanticEmbedder {
    model: TextEmbedding,
    batch_size: usize,
}

#[cfg(not(ctx_semantic_fastembed))]
struct SemanticEmbedder;

#[cfg(ctx_semantic_fastembed)]
fn new_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    let snapshot = semantic_model_cache_snapshot_dir(cache_dir).ok_or_else(|| {
        anyhow!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        )
    })?;
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::AllMiniLML6V2)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: fs::read(snapshot.join("tokenizer.json"))
            .with_context(|| format!("read semantic tokenizer.json from {}", snapshot.display()))?,
        config_file: fs::read(snapshot.join("config.json"))
            .with_context(|| format!("read semantic config.json from {}", snapshot.display()))?,
        special_tokens_map_file: fs::read(snapshot.join("special_tokens_map.json")).with_context(
            || {
                format!(
                    "read semantic special_tokens_map.json from {}",
                    snapshot.display()
                )
            },
        )?,
        tokenizer_config_file: fs::read(snapshot.join("tokenizer_config.json")).with_context(
            || {
                format!(
                    "read semantic tokenizer_config.json from {}",
                    snapshot.display()
                )
            },
        )?,
    };
    let mut user_model = UserDefinedEmbeddingModel::new(
        fs::read(snapshot.join(&model_info.model_file)).with_context(|| {
            format!(
                "read semantic model file {} from {}",
                model_info.model_file,
                snapshot.display()
            )
        })?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::AllMiniLML6V2)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::AllMiniLML6V2,
    ));
    user_model.output_key = model_info.output_key.clone();
    let options = InitOptionsUserDefined::new().with_intra_threads(semantic_embedder_threads());
    let model = TextEmbedding::try_new_from_user_defined(user_model, options)
        .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))?;
    Ok(SemanticEmbedder {
        model,
        batch_size: semantic_embed_batch_size(),
    })
}

#[cfg(not(ctx_semantic_fastembed))]
fn new_semantic_embedder(_cache_dir: &Path) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedder_threads() -> usize {
    env_usize("CTX_SEMANTIC_THREADS")
        .map(|value| value.min(SEMANTIC_EMBED_THREADS_MAX))
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|threads| threads.get().clamp(1, SEMANTIC_EMBED_THREADS_DEFAULT))
        })
        .unwrap_or(SEMANTIC_EMBED_THREADS_DEFAULT)
}

#[cfg(ctx_semantic_fastembed)]
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
    semantic_model_cache_snapshot_dir(cache_dir).is_some()
}

fn semantic_model_cache_snapshot_dir(cache_dir: &Path) -> Option<PathBuf> {
    if !semantic_embedding_supported() {
        return None;
    }
    if cache_dir.as_os_str().is_empty() {
        return None;
    }
    let model_root = cache_dir.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let snapshot_ref = fs::read_to_string(model_root.join("refs").join("main")).ok()?;
    let snapshot_ref = snapshot_ref.trim();
    if snapshot_ref.is_empty()
        || snapshot_ref.contains('/')
        || snapshot_ref.contains('\\')
        || snapshot_ref == "."
        || snapshot_ref == ".."
    {
        return None;
    }
    let snapshot = model_root.join("snapshots").join(snapshot_ref);
    if !snapshot.is_dir() {
        return None;
    }
    if SEMANTIC_REQUIRED_MODEL_FILES.iter().all(|file| {
        fs::metadata(snapshot.join(file))
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
    }) {
        Some(snapshot)
    } else {
        None
    }
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedding_supported() -> bool {
    true
}

#[cfg(not(ctx_semantic_fastembed))]
fn semantic_embedding_supported() -> bool {
    false
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}
