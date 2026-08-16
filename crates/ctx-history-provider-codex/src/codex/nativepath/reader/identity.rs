use super::*;

pub(super) fn probe_timestamp(
    probe: &CodexRecordProbe<'_>,
    fallback: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match probe.timestamp.as_deref() {
        Some(timestamp) => DateTime::parse_from_rfc3339(timestamp)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc)),
        None => Some(fallback),
    }
}
