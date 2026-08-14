use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::normalization::provider_timestamp_seconds;

pub fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// Parses the timestamp spellings shared by SQLite-backed provider schemas.
pub fn parse_provider_timestamp(value: &str, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let value = value.trim();
    if value.is_empty() {
        return fallback;
    }
    parse_rfc3339_utc(value)
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .or_else(|| {
            value
                .parse::<f64>()
                .ok()
                .map(|seconds| provider_timestamp_seconds(Some(seconds), fallback))
        })
        .unwrap_or(fallback)
}

pub fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
