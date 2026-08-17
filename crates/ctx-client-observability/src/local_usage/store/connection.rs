use std::time::Duration;

use rusqlite::Connection;

use super::{
    UsageStoreError, BUSY_TIMEOUT, JOURNAL_SIZE_LIMIT_BYTES, MAX_PAGE_COUNT,
    WAL_AUTOCHECKPOINT_PAGES,
};

pub(super) fn configure_persistent(conn: &Connection) -> Result<(), UsageStoreError> {
    conn.pragma_update(None, "max_page_count", MAX_PAGE_COUNT)?;
    let max_page_count: i64 = conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
    if max_page_count > MAX_PAGE_COUNT {
        return Err(UsageStoreError::GrowthLimit);
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    Ok(())
}

pub(super) fn configure_transient(
    conn: &Connection,
    busy_timeout: Duration,
) -> Result<(), UsageStoreError> {
    conn.busy_timeout(busy_timeout)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    Ok(())
}

pub(super) fn configure_report_connection(conn: &Connection) -> Result<(), UsageStoreError> {
    configure_transient(conn, BUSY_TIMEOUT)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(())
}
