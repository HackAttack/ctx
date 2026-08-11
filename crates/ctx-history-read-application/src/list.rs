use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::DateTime;
use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use ctx_history_index_query::{
    CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeError, CoreEventRangeFilters,
    CoreEventRangePage, CoreEventRangeSelection,
};
use uuid::Uuid;

use crate::PinnedHistoryQuery;

pub const DEFAULT_EVENT_QUERY_LIMIT: u64 = 10_000;
pub const MAX_EVENT_QUERY_LIMIT: u64 = 10_000_000;
pub const MAX_EVENT_QUERY_CURSOR_CHARS: usize = 512;

#[derive(Debug, Clone)]
pub struct ListEventsRequest {
    pub since: Option<String>,
    pub until: Option<String>,
    pub filters: CoreEventRangeFilters,
    pub cursor: Option<CoreEventRangeCursor>,
    pub limit: u64,
    pub page_items: usize,
    pub byte_budget: usize,
    pub strict_budget: Option<CoreEventPageBudget>,
}

#[derive(Debug)]
pub struct ListEventsResult {
    pub selection: CoreEventRangeSelection,
    pub limit: usize,
    pub page: CoreEventRangePage,
}

#[derive(Debug, Clone)]
pub struct ListEventsPageRequest {
    pub selection: CoreEventRangeSelection,
    pub cursor: Option<CoreEventRangeCursor>,
    pub limit: u64,
    pub page_items: usize,
    pub byte_budget: usize,
    pub strict_budget: Option<CoreEventPageBudget>,
}

#[derive(Debug, thiserror::Error)]
pub enum ListEventsError {
    #[error(transparent)]
    Range(#[from] CoreEventRangeError),
    #[error("{field} must be an absolute RFC3339 timestamp: {value:?}")]
    InvalidTimestamp { field: &'static str, value: String },
    #[error("{field} must resolve exactly to a whole millisecond: {value:?}")]
    InvalidTimestampPrecision { field: &'static str, value: String },
    #[error("since and until must be supplied together")]
    IncompleteTimestampRange,
    #[error("{field} must be a full UUID: {value:?}")]
    InvalidUuid { field: &'static str, value: String },
    #[error("{field} value {requested} is outside {minimum}..={maximum}")]
    InvalidResourceLimit {
        field: &'static str,
        requested: u64,
        minimum: u64,
        maximum: u64,
    },
    #[error("event query cursor is {actual} characters, maximum {maximum}")]
    CursorTooLarge { actual: usize, maximum: usize },
    #[error("event query cursor is not canonical base64url")]
    InvalidCursorEncoding,
}

impl PinnedHistoryQuery<'_> {
    pub fn list_events(
        &self,
        request: &ListEventsRequest,
    ) -> Result<ListEventsResult, ListEventsError> {
        let selection = event_range_selection(
            request.since.as_deref(),
            request.until.as_deref(),
            request.filters.clone(),
        )?;
        self.list_events_page(&ListEventsPageRequest {
            selection,
            cursor: request.cursor.clone(),
            limit: request.limit,
            page_items: request.page_items,
            byte_budget: request.byte_budget,
            strict_budget: request.strict_budget,
        })
    }

    pub fn list_events_page(
        &self,
        request: &ListEventsPageRequest,
    ) -> Result<ListEventsResult, ListEventsError> {
        if let Some(cursor) = request.cursor.as_ref() {
            cursor.validate_selection(&request.selection)?;
        }
        let limit = validated_event_limit(request.limit)?;
        let page_items = request.page_items.min(limit);
        let budget = CoreEventPageBudget::new(
            request.byte_budget,
            request.byte_budget.min(MAX_CORE_CONTENT_BYTES),
        );
        let page = match request.strict_budget {
            Some(strict_budget) => self.index.core_event_range_page_with_strict_budget(
                &request.selection,
                request.cursor.as_ref(),
                page_items,
                budget,
                strict_budget,
            ),
            None => self.index.core_event_range_page_with_budget(
                &request.selection,
                request.cursor.as_ref(),
                page_items,
                budget,
            ),
        }?;
        Ok(ListEventsResult {
            selection: request.selection.clone(),
            limit,
            page,
        })
    }
}

pub fn event_range_selection(
    since: Option<&str>,
    until: Option<&str>,
    filters: CoreEventRangeFilters,
) -> Result<CoreEventRangeSelection, ListEventsError> {
    match (since, until) {
        (None, None) => CoreEventRangeSelection::all(filters).map_err(Into::into),
        (Some(since), Some(until)) => CoreEventRangeSelection::with_filters(
            parse_rfc3339("since", since)?,
            parse_rfc3339("until", until)?,
            filters,
        )
        .map_err(Into::into),
        (Some(_), None) | (None, Some(_)) => Err(ListEventsError::IncompleteTimestampRange),
    }
}

pub fn validated_event_limit(limit: u64) -> Result<usize, ListEventsError> {
    if !(1..=MAX_EVENT_QUERY_LIMIT).contains(&limit) {
        return Err(ListEventsError::InvalidResourceLimit {
            field: "limit",
            requested: limit,
            minimum: 1,
            maximum: MAX_EVENT_QUERY_LIMIT,
        });
    }
    usize::try_from(limit).map_err(|_| ListEventsError::InvalidResourceLimit {
        field: "limit",
        requested: limit,
        minimum: 1,
        maximum: MAX_EVENT_QUERY_LIMIT,
    })
}

pub fn decode_event_range_cursor(encoded: &str) -> Result<CoreEventRangeCursor, ListEventsError> {
    if encoded.len() > MAX_EVENT_QUERY_CURSOR_CHARS {
        return Err(ListEventsError::CursorTooLarge {
            actual: encoded.len(),
            maximum: MAX_EVENT_QUERY_CURSOR_CHARS,
        });
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ListEventsError::InvalidCursorEncoding)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(ListEventsError::InvalidCursorEncoding);
    }
    Ok(CoreEventRangeCursor::decode(&bytes)?)
}

pub fn encode_event_range_cursor(cursor: &CoreEventRangeCursor) -> String {
    URL_SAFE_NO_PAD.encode(cursor.encode())
}

pub fn parse_event_query_uuid(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<Uuid>, ListEventsError> {
    value
        .map(|value| {
            Uuid::parse_str(value).map_err(|_| ListEventsError::InvalidUuid {
                field,
                value: value.to_owned(),
            })
        })
        .transpose()
}

fn parse_rfc3339(field: &'static str, value: &str) -> Result<i64, ListEventsError> {
    let timestamp =
        DateTime::parse_from_rfc3339(value).map_err(|_| ListEventsError::InvalidTimestamp {
            field,
            value: value.to_owned(),
        })?;
    if timestamp.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(ListEventsError::InvalidTimestampPrecision {
            field,
            value: value.to_owned(),
        });
    }
    Ok(timestamp.timestamp_millis())
}
