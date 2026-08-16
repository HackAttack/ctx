mod content_policy;
mod result_content;
mod value;

pub use content_policy::{
    provider_policy_body, provider_policy_event_text, ProviderPolicyText, ProviderTextRetention,
};
pub use result_content::provider_normalized_result_value;
pub use value::{
    capped_text, provider_block_event_type, provider_block_text, provider_capped_json,
    provider_capped_json_value, provider_explicit_result_value_text, provider_json_text,
    provider_line_from_index, provider_local_preview, provider_message_has_part_kind,
    provider_message_id, provider_message_parts, provider_part_text, provider_role,
    provider_role_from_message, provider_string_field, provider_timestamp_from_fields,
    provider_timestamp_millis, provider_timestamp_seconds, provider_timestamp_seconds_to_datetime,
    provider_timestamp_value, provider_value_text, text_id_index,
};

use chrono::{DateTime, Utc};

/// Converts a provider-native signed integer into its nonnegative Core form.
///
/// Provider packs keep the field label so their stable validation text stays
/// provider-owned without depending on the capture facade's error type.
pub fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{field} must be nonnegative, got {value}"))
}

/// Requires one provider-native seconds timestamp to be representable by Core.
pub fn provider_required_timestamp_seconds(
    value: f64,
    field: &'static str,
) -> Result<DateTime<Utc>, String> {
    provider_timestamp_seconds_to_datetime(value)
        .ok_or_else(|| format!("{field} is outside representable timestamp range: {value}"))
}

#[cfg(test)]
mod tests;
