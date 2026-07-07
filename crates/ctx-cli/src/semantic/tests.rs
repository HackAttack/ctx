#[cfg(all(test, ctx_sqlite_vec))]
mod tests {
    use super::*;

    fn test_embedding(first: f32, second: f32) -> Vec<f32> {
        let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
        embedding[0] = first;
        embedding[1] = second;
        embedding
    }

    fn test_chunk(event_id: Uuid, seq: u64, source_hash: &str) -> SemanticChunkDocument {
        test_chunk_at(event_id, seq, source_hash, 0, 1)
    }

    fn test_chunk_at(
        event_id: Uuid,
        seq: u64,
        source_hash: &str,
        chunk_index: usize,
        chunk_count: usize,
    ) -> SemanticChunkDocument {
        SemanticChunkDocument {
            event_id,
            history_record_id: None,
            session_id: None,
            seq,
            chunk_index,
            chunk_count,
            source_text_hash: source_hash.to_owned(),
            chunk_text_hash: format!("{source_hash}-chunk-{chunk_index}"),
            text: String::new(),
            start_char: chunk_index.saturating_mul(10),
            end_char: chunk_index.saturating_mul(10).saturating_add(12),
        }
    }

    #[test]
    fn sqlite_vec0_full_scan_matches_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        assert!(store.sqlite_vec0_ready()?);

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[close_event, far_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(rust_hits.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(rust_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, close_event);
        assert_eq!(rust_hits.hits[0].event_id, close_event);
        assert_eq!(sqlite_hits.hits[1].event_id, far_event);
        assert_eq!(rust_hits.hits[1].event_id, far_event);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_caps_large_k_without_falling_back() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;

        let search = store.search(&test_embedding(1.0, 0.0), SEMANTIC_SQLITE_VEC0_MAX_K + 1)?;

        assert_eq!(
            search.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(search.hits.len(), 2);
        assert_eq!(search.hits[0].event_id, close_event);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_overfetches_until_unique_events_match_rust_scan() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let multi_chunk_event = Uuid::new_v4();
        let next_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 0, 3),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 1, 3),
                test_embedding(0.999, 0.044),
            ),
            (
                test_chunk_at(multi_chunk_event, 2, "multi", 2, 3),
                test_embedding(0.995, 0.099),
            ),
            (
                test_chunk_at(next_event, 1, "next", 0, 1),
                test_embedding(0.98, 0.199),
            ),
        ])?;

        let query = test_embedding(1.0, 0.0);
        let sqlite_hits = store.search(&query, 2)?;
        let rust_hits = store.search_event_ids(&query, &[multi_chunk_event, next_event], 2)?;

        assert_eq!(
            sqlite_hits.stats.backend,
            Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
        );
        assert_eq!(sqlite_hits.hits.len(), 2);
        assert_eq!(sqlite_hits.hits[0].event_id, multi_chunk_event);
        assert_eq!(sqlite_hits.hits[1].event_id, next_event);
        assert_eq!(
            sqlite_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            rust_hits
                .hits
                .iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_incompatible_derived_schema() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let vector_path = temp.path().join("vectors.sqlite");
        {
            let conn = Connection::open(&vector_path)?;
            conn.execute_batch(
                r#"
                CREATE TABLE event_embedding_vec0_meta (
                    rowid INTEGER PRIMARY KEY,
                    event_id TEXT NOT NULL
                );
                CREATE TABLE event_embedding_vec0 (
                    rowid INTEGER PRIMARY KEY,
                    embedding BLOB
                );
                "#,
            )?;
        }

        let mut store = SemanticVectorStore::open(&vector_path)?;
        let close_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[(
            test_chunk(close_event, 1, "close"),
            test_embedding(1.0, 0.0),
        )])?;

        assert!(store.sqlite_vec0_ready()?);
        let vec0_sql = sqlite_table_sql(&store.conn, "event_embedding_vec0")?.unwrap_or_default();
        assert!(vec0_sql.to_ascii_lowercase().contains("using vec0"));
        assert!(sqlite_table_has_columns(
            &store.conn,
            "event_embedding_vec0_meta",
            &["model_key", "source_text_sha256", "start_char", "end_char"]
        )?);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_rebuilds_when_same_count_meta_rowids_drift() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let canonical_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
	            "UPDATE event_embedding_vec0_meta SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2",
	            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
	        )?;

        assert!(!store.sqlite_vec0_ready()?);
        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);

        let repaired_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_vec0_meta WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(repaired_rowid, canonical_rowid);
        Ok(())
    }

    #[test]
    fn sqlite_vec0_payload_drift_falls_back_and_rebuilds() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
        let close_event = Uuid::new_v4();
        let far_event = Uuid::new_v4();
        store.upsert_chunk_embeddings(&[
            (
                test_chunk(close_event, 2, "close"),
                test_embedding(1.0, 0.0),
            ),
            (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
        ])?;
        assert!(store.sqlite_vec0_ready()?);

        let close_rowid = store.conn.query_row(
            "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            params![close_event.to_string(), SEMANTIC_MODEL_KEY],
            |row| row.get::<_, i64>(0),
        )?;
        store.conn.execute(
            "DELETE FROM event_embedding_vec0 WHERE rowid = ?1",
            params![close_rowid],
        )?;
        store.conn.execute(
            "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
            params![close_rowid, serialize_f32_blob(&test_embedding(0.0, 1.0))],
        )?;

        assert!(!store.sqlite_vec0_ready()?);
        let search = store.search(&test_embedding(1.0, 0.0), 2)?;
        assert_eq!(search.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
        assert_eq!(search.hits[0].event_id, close_event);

        store.sync_sqlite_vec0_from_chunks_if_needed()?;
        assert!(store.sqlite_vec0_ready()?);
        Ok(())
    }

    #[test]
    fn daemon_autostart_records_lifecycle_trigger_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let args = DaemonRunArgs {
            foreground: false,
            once: true,
            max_runtime_seconds: None,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Setup),
            json: true,
        };

        write_daemon_lifecycle_status(temp.path(), &args, "running", 123, None, None)?;
        let status = read_daemon_status(temp.path()).expect("daemon status");
        assert_eq!(status["start_mode"], "auto");
        assert_eq!(status["trigger_command"], "setup");
        Ok(())
    }
}
