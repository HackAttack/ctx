use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::schema::ddl::{table_exists, CREATE_TABLES_SQL};
use crate::Result;

pub(crate) fn rebuild_v44_current_schema_tables(conn: &Connection) -> Result<()> {
    for table in [
        "capture_sources",
        "vcs_workspaces",
        "history_records",
        "artifacts",
        "sessions",
        "session_edges",
        "runs",
        "events",
        "vcs_changes",
        "history_record_links",
        "summaries",
        "files_touched",
        "record_edges",
        "sync_outbox",
    ] {
        rebuild_table_from_current_schema(conn, table)?;
    }
    Ok(())
}

pub(crate) fn rebuild_table_from_current_schema(conn: &Connection, table: &str) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    let new_table = format!("{table}_new");
    conn.execute(&format!("DROP TABLE IF EXISTS {new_table}"), [])?;
    conn.execute_batch(&create_table_rebuild_sql(table, &new_table)?)?;

    let old_columns = table_columns(conn, table)?;
    let old_column_set = old_columns.iter().cloned().collect::<BTreeSet<_>>();
    let new_columns = table_columns(conn, &new_table)?
        .into_iter()
        .filter(|column| old_column_set.contains(column))
        .collect::<Vec<_>>();
    if !new_columns.is_empty() {
        let column_list = new_columns.join(", ");
        let select_list = column_list.clone();
        conn.execute(
            &format!("INSERT INTO {new_table} ({column_list}) SELECT {select_list} FROM {table}"),
            [],
        )?;
    }
    conn.execute(&format!("DROP TABLE {table}"), [])?;
    conn.execute(&format!("ALTER TABLE {new_table} RENAME TO {table}"), [])?;
    Ok(())
}

fn create_table_rebuild_sql(table: &str, new_table: &str) -> Result<String> {
    let marker = format!("CREATE TABLE IF NOT EXISTS {table}");
    let start = CREATE_TABLES_SQL
        .find(&marker)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let rest = &CREATE_TABLES_SQL[start..];
    let end = rest.find("\n);").ok_or(rusqlite::Error::InvalidQuery)? + 3;
    Ok(rest[..end].replacen(&marker, &format!("CREATE TABLE {new_table}"), 1))
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}
