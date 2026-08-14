mod io;

pub(crate) use io::provider_optional_regular_file;

use chrono::{DateTime, Utc};

use crate::{CaptureError, Result};

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
