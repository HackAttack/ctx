use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};
use thiserror::Error;

use ctx_history_core::utc_now;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SourceIdentityFilterError {
    #[error("history_source expects plugin/source or provider_key/source_id")]
    InvalidHistorySource,
    #[error("custom history source filters require the custom provider")]
    CustomProviderRequired,
}

#[derive(Debug, Clone, Default)]
pub struct SourceIdentityFilterArgs {
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SourceIdentityFilters {
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
}

impl SourceIdentityFilters {
    pub fn is_empty(&self) -> bool {
        self.history_source.is_none()
            && self.provider_key.is_none()
            && self.source_id.is_none()
            && self.source_format.is_none()
    }
}

pub fn normalize_source_identity_filters(
    input: SourceIdentityFilterArgs,
) -> Result<SourceIdentityFilters> {
    let history_source = normalize_source_identity_filter("history_source", input.history_source)?;
    if history_source
        .as_deref()
        .is_some_and(|value| !value.contains('/'))
    {
        return Err(SourceIdentityFilterError::InvalidHistorySource.into());
    }
    Ok(SourceIdentityFilters {
        history_source,
        provider_key: normalize_source_identity_filter("provider_key", input.provider_key)?,
        source_id: normalize_source_identity_filter("source_id", input.source_id)?,
        source_format: normalize_source_identity_filter("source_format", input.source_format)?,
    })
}

pub fn normalize_source_identity_filter(
    label: &str,
    value: Option<String>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(anyhow!("{label} cannot contain control characters"));
    }
    Ok(Some(value.to_owned()))
}

pub fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days
            .parse()
            .with_context(|| format!("invalid since day window: {value}"))?;
        let duration = Duration::try_days(days)
            .ok_or_else(|| anyhow!("invalid since day window: {value}: value too large"))?;
        let since = utc_now()
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow!("invalid since day window: {value}: value too large"))?;
        return Ok(since);
    }
    Ok(chrono::DateTime::parse_from_rfc3339(trimmed)
        .with_context(|| format!("invalid since value: {value}"))?
        .with_timezone(&Utc))
}
