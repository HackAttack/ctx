mod io;

pub(crate) use io::provider_optional_regular_file;

use chrono::{DateTime, Utc};

use crate::{CaptureError, Result};

pub(crate) fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} must be nonnegative, got {value}"))
    })
}

pub(crate) fn provider_required_timestamp_seconds(
    value: f64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    ctx_history_capture_model::normalization::provider_timestamp_seconds_to_datetime(value)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "{field} is outside representable timestamp range: {value}"
            ))
        })
}

pub(crate) fn provider_required_timestamp_millis(
    value: i64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}
