use rusqlite::Connection;

use crate::schema::ddl::{table_exists, table_has_column};
use crate::schema::fts::{create_fts_tables_if_supported, drop_fts_table_if_exists};
use crate::schema::indexes::INDEXES_SQL;
use crate::search::projections::{
    rebuild_event_search_lookup_projection, rebuild_search_projection,
};
use crate::{Result, StoreError};

pub(crate) fn migrate_to_v45(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        drop_fts_table_if_exists(conn, "ctx_history_search_scriptgram")?;
        drop_fts_table_if_exists(conn, "event_search_scriptgram")?;
        create_fts_tables_if_supported(conn)?;
        if table_exists(conn, "event_search_lookup")?
            && !table_has_column(conn, "event_search_lookup", "history_record_id")?
        {
            conn.execute_batch("DROP TABLE event_search_lookup;")?;
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS event_search_lookup (
                event_id TEXT PRIMARY KEY NOT NULL REFERENCES events(id) ON DELETE CASCADE,
                history_record_id TEXT REFERENCES history_records(id),
                session_id TEXT REFERENCES sessions(id),
                role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
                preview_text TEXT NOT NULL,
                rank_bucket TEXT NOT NULL
            );
            "#,
        )?;
        conn.execute_batch(INDEXES_SQL)?;
        rebuild_search_projection(conn)?;
        rebuild_event_search_lookup_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 45;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}
