use std::{collections::BTreeSet, fmt, ops::Range};

use anyhow::{anyhow, Result};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index_format::project_body_search;
use ctx_history_index_query::{CoreEventPageBudget, CoreEventRecord, VerifiedIndex};
use unicode_segmentation::UnicodeSegmentation as _;
use uuid::Uuid;

use crate::{NormalizedSearchQuery, SearchEventMetadata, SearchHit};

pub const MAX_SEARCH_RESULTS: usize = 200;
pub const SEARCH_SNIPPET_MAX_CHARS: usize = 320;
pub const SEARCH_SNIPPET_MAX_BYTES: usize = 16 * 1024;

const SEARCH_CORE_RECORD_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(MAX_ENCODED_CORE_RECORD_BYTES, MAX_CORE_CONTENT_BYTES);
pub const SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES: usize =
    MAX_SEARCH_RESULTS * SEARCH_SNIPPET_MAX_BYTES;

/// Bounded query result state derived from one complete stored Core record.
#[derive(Debug, PartialEq, Eq)]
pub struct SearchPresentation {
    pub event_id: Uuid,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPresentationHydrationBudget {
    pub maximum_retained_snippet_bytes: usize,
}

pub const SEARCH_PRESENTATION_HYDRATION_BUDGET: SearchPresentationHydrationBudget =
    SearchPresentationHydrationBudget {
        maximum_retained_snippet_bytes: SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
    };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPresentationRetentionBudgetExceeded {
    pub event_id: Uuid,
    pub retained_snippet_bytes: usize,
    pub maximum_retained_snippet_bytes: usize,
}

impl fmt::Display for SearchPresentationRetentionBudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core search event {} cannot fit the bounded search presentation retention budget (retained snippets: {}/{})",
            self.event_id,
            self.retained_snippet_bytes,
            self.maximum_retained_snippet_bytes,
        )
    }
}

impl std::error::Error for SearchPresentationRetentionBudgetExceeded {}

pub(crate) fn presentations_for_search_hits(
    index: &VerifiedIndex,
    hits: &[SearchHit],
    query: &NormalizedSearchQuery,
) -> Result<Vec<SearchPresentation>> {
    presentations_for_search_hits_with_budget(
        index,
        hits,
        query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
}

pub fn presentations_for_search_hits_with_budget(
    index: &VerifiedIndex,
    hits: &[SearchHit],
    query: &NormalizedSearchQuery,
    budget: SearchPresentationHydrationBudget,
) -> Result<Vec<SearchPresentation>> {
    if budget.maximum_retained_snippet_bytes == 0 {
        return Err(anyhow!(
            "search presentation hydration budget must be positive"
        ));
    }

    let mut requested = BTreeSet::new();
    for hit in hits {
        if !requested.insert(hit.event.event_id) {
            return Err(anyhow!(
                "search result duplicated Core event {}",
                hit.event.event_id
            ));
        }
    }

    let event_ids = hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();
    // Execute one generation-pinned Tantivy selection. The returned iterator
    // decodes exactly one complete Core record at a time, allowing each body
    // to be projected and discarded before the next record is materialized.
    let mut records = index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &event_ids,
            hits.len(),
            SEARCH_CORE_RECORD_BUDGET,
        )?
        .ok_or_else(|| {
            anyhow!(
                "pinned Core lookup omitted search event {}",
                event_ids.first().copied().unwrap_or_else(Uuid::nil)
            )
        })?;
    let query_texts = query.texts();
    let mut presentations = Vec::with_capacity(hits.len());
    let mut retained_snippet_bytes = 0_usize;
    for hit in hits {
        let event_id = hit.event.event_id;
        let record = records
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("pinned Core lookup omitted search event {event_id}"))?;

        let (presentation, snippet_bytes) =
            search_presentation_projection(record, &hit.event, &query_texts)?;
        let next_retained_snippet_bytes = retained_snippet_bytes
            .checked_add(snippet_bytes)
            .ok_or_else(|| {
                search_presentation_retention_budget_error(event_id, retained_snippet_bytes, budget)
            })?;
        if next_retained_snippet_bytes > budget.maximum_retained_snippet_bytes {
            return Err(search_presentation_retention_budget_error(
                event_id,
                next_retained_snippet_bytes,
                budget,
            ));
        }
        retained_snippet_bytes = next_retained_snippet_bytes;
        presentations.push(presentation);
    }
    if records.next().transpose()?.is_some() {
        return Err(anyhow!(
            "pinned Core lookup returned more search records than requested"
        ));
    }
    Ok(presentations)
}

fn search_presentation_projection(
    record: CoreEventRecord,
    expected_event: &SearchEventMetadata,
    query_texts: &[&str],
) -> Result<(SearchPresentation, usize)> {
    let CoreEventRecord { event, core_record } = record;
    if event.event_id != core_record.event_id
        || event.session_id != core_record.session_id
        || SearchEventMetadata::from(&event) != *expected_event
    {
        return Err(anyhow!(
            "pinned Core lookup returned misaligned metadata for search event {}",
            expected_event.event_id
        ));
    }
    let body = project_body_search(core_record.content)?.ok_or_else(|| {
        anyhow!(
            "Core search event {} has no searchable body projection",
            event.event_id
        )
    })?;
    let (snippet, snippet_truncated) = search_snippet_fragment(&body, query_texts);
    let retained_snippet_bytes = snippet.len();

    // Neither the complete searchable projection nor the remainder of Core
    // crosses the search presentation boundary.
    drop(body);
    drop(event);
    Ok((
        SearchPresentation {
            event_id: expected_event.event_id,
            snippet,
            snippet_truncated,
        },
        retained_snippet_bytes,
    ))
}

fn search_presentation_retention_budget_error(
    event_id: Uuid,
    retained_snippet_bytes: usize,
    budget: SearchPresentationHydrationBudget,
) -> anyhow::Error {
    anyhow::Error::new(SearchPresentationRetentionBudgetExceeded {
        event_id,
        retained_snippet_bytes,
        maximum_retained_snippet_bytes: budget.maximum_retained_snippet_bytes,
    })
}

pub fn search_snippet_fragment(body: &str, query_texts: &[&str]) -> (String, bool) {
    let direct_ascii_offsets = ascii_grapheme_offsets_are_bytes(body);
    let grapheme_count = if direct_ascii_offsets {
        body.len()
    } else {
        body.graphemes(true).count()
    };
    if grapheme_count <= SEARCH_SNIPPET_MAX_CHARS {
        return byte_bounded_search_snippet(body, query_texts, false);
    }

    let start = query_match_range(body, query_texts).map_or(0, |matched| {
        if direct_ascii_offsets {
            centered_snippet_start_from_match(grapheme_count, matched.start, matched.end)
        } else {
            centered_snippet_start(body, grapheme_count, matched)
        }
    });
    let end = start.saturating_add(SEARCH_SNIPPET_MAX_CHARS);
    let byte_range = if direct_ascii_offsets {
        start..end
    } else {
        grapheme_byte_range(body, start, end)
    };
    let snippet = &body[byte_range];
    let truncated = start > 0 || end < grapheme_count;
    byte_bounded_search_snippet(snippet, query_texts, truncated)
}

fn byte_bounded_search_snippet(
    snippet: &str,
    query_texts: &[&str],
    truncated: bool,
) -> (String, bool) {
    if snippet.len() <= SEARCH_SNIPPET_MAX_BYTES {
        return (snippet.to_owned(), truncated);
    }

    let graphemes = snippet
        .grapheme_indices(true)
        .map(|(start, grapheme)| start..start.saturating_add(grapheme.len()))
        .collect::<Vec<_>>();
    let matched = query_match_range(snippet, query_texts);
    let window = match matched.as_ref() {
        Some(matched) => grapheme_span_covering_match(&graphemes, matched)
            .filter(|required| {
                grapheme_window_bytes(&graphemes, required) <= SEARCH_SNIPPET_MAX_BYTES
            })
            .and_then(|required| {
                match_containing_grapheme_window(&graphemes, &required, Some(matched))
            }),
        None => fallback_grapheme_window(&graphemes, None),
    };
    let Some(window) = window else {
        return (String::new(), true);
    };
    (snippet[window].to_owned(), true)
}

fn match_containing_grapheme_window(
    graphemes: &[Range<usize>],
    required: &Range<usize>,
    matched: Option<&Range<usize>>,
) -> Option<Range<usize>> {
    let required_center = matched.map_or_else(
        || {
            graphemes[required.start]
                .start
                .saturating_add(graphemes[required.end - 1].end)
        },
        |matched| matched.start.saturating_add(matched.end),
    );
    let mut best: Option<(Range<usize>, usize, usize)> = None;
    for start in 0..=required.start {
        for end in required.end..=graphemes.len() {
            let bytes = graphemes[end - 1]
                .end
                .saturating_sub(graphemes[start].start);
            if bytes > SEARCH_SNIPPET_MAX_BYTES {
                break;
            }
            let center = graphemes[start]
                .start
                .saturating_add(graphemes[end - 1].end);
            let center_distance = center.abs_diff(required_center);
            let replace = best
                .as_ref()
                .is_none_or(|(best_range, best_bytes, best_distance)| {
                    bytes > *best_bytes
                        || (bytes == *best_bytes && center_distance < *best_distance)
                        || (bytes == *best_bytes
                            && center_distance == *best_distance
                            && graphemes[start].start < best_range.start)
                });
            if replace {
                best = Some((
                    graphemes[start].start..graphemes[end - 1].end,
                    bytes,
                    center_distance,
                ));
            }
        }
    }
    best.map(|(window, _, _)| window)
}

fn fallback_grapheme_window(
    graphemes: &[Range<usize>],
    matched: Option<&Range<usize>>,
) -> Option<Range<usize>> {
    let match_center = matched.map(|matched| matched.start.saturating_add(matched.end));
    let mut best: Option<(Range<usize>, usize, usize)> = None;
    for start in 0..graphemes.len() {
        for end in start.saturating_add(1)..=graphemes.len() {
            let bytes = graphemes[end - 1]
                .end
                .saturating_sub(graphemes[start].start);
            if bytes > SEARCH_SNIPPET_MAX_BYTES {
                break;
            }
            let window = graphemes[start].start..graphemes[end - 1].end;
            let match_distance = match_center.map_or(0, |center| {
                if center < window.start.saturating_mul(2) {
                    window.start.saturating_mul(2).saturating_sub(center)
                } else if center > window.end.saturating_mul(2) {
                    center.saturating_sub(window.end.saturating_mul(2))
                } else {
                    0
                }
            });
            let replace = best
                .as_ref()
                .is_none_or(|(best_window, best_bytes, best_distance)| {
                    (matched.is_some() && match_distance < *best_distance)
                        || (matched.is_some()
                            && match_distance == *best_distance
                            && bytes > *best_bytes)
                        || (matched.is_none() && bytes > *best_bytes)
                        || (match_distance == *best_distance
                            && bytes == *best_bytes
                            && window.start < best_window.start)
                });
            if replace {
                best = Some((window, bytes, match_distance));
            }
        }
    }
    best.map(|(window, _, _)| window)
}

fn grapheme_window_bytes(graphemes: &[Range<usize>], window: &Range<usize>) -> usize {
    graphemes[window.end - 1]
        .end
        .saturating_sub(graphemes[window.start].start)
}

fn grapheme_span_covering_match(
    graphemes: &[Range<usize>],
    matched: &Range<usize>,
) -> Option<Range<usize>> {
    let start = graphemes
        .iter()
        .position(|grapheme| grapheme.end > matched.start)?;
    let end = graphemes
        .iter()
        .rposition(|grapheme| grapheme.start < matched.end)?
        .saturating_add(1);
    (start < end).then_some(start..end)
}

fn centered_snippet_start(body: &str, grapheme_count: usize, matched: Range<usize>) -> usize {
    let mut match_start = 0;
    let mut match_end = 0;
    for (index, (offset, _)) in body.grapheme_indices(true).enumerate() {
        if offset <= matched.start {
            match_start = index;
        }
        if offset < matched.end {
            match_end = index.saturating_add(1);
        } else {
            break;
        }
    }
    let match_start = match_start.min(grapheme_count.saturating_sub(1));
    let match_end = match_end.min(grapheme_count);
    centered_snippet_start_from_match(grapheme_count, match_start, match_end)
}

fn centered_snippet_start_from_match(
    grapheme_count: usize,
    match_start: usize,
    match_end: usize,
) -> usize {
    let latest_start = grapheme_count.saturating_sub(SEARCH_SNIPPET_MAX_CHARS);
    let match_graphemes = match_end.saturating_sub(match_start).max(1);
    let leading_context = SEARCH_SNIPPET_MAX_CHARS
        .saturating_sub(match_graphemes)
        .saturating_div(2);
    match_start
        .saturating_sub(leading_context)
        .min(latest_start)
}

fn ascii_grapheme_offsets_are_bytes(body: &str) -> bool {
    let mut previous_was_carriage_return = false;
    for byte in body.bytes() {
        if !byte.is_ascii() || (previous_was_carriage_return && byte == b'\n') {
            return false;
        }
        previous_was_carriage_return = byte == b'\r';
    }
    true
}

fn grapheme_byte_range(body: &str, start: usize, end: usize) -> Range<usize> {
    let mut start_offset = None;
    let mut end_offset = None;
    for (index, (offset, _)) in body.grapheme_indices(true).enumerate() {
        if index == start {
            start_offset = Some(offset);
        }
        if index == end {
            end_offset = Some(offset);
            break;
        }
    }
    start_offset.unwrap_or(body.len())..end_offset.unwrap_or(body.len())
}

fn query_match_range(body: &str, query_texts: &[&str]) -> Option<Range<usize>> {
    let folded_body = if body.is_ascii() {
        body.to_ascii_lowercase()
    } else {
        body.to_lowercase()
    };
    let mut best_full_match = None;
    for query_text in query_texts {
        let query_text = query_text.trim();
        if query_text.is_empty() {
            continue;
        }
        update_preferred_match(
            &mut best_full_match,
            folded_match_range(body, &folded_body, query_text),
            query_text.chars().count(),
        );
    }
    if let Some((_, matched)) = best_full_match {
        return Some(matched);
    }

    let mut best_term_match = None;
    for query_text in query_texts {
        let query_text = query_text.trim();
        for term in query_text.split(|character: char| !character.is_alphanumeric()) {
            if term.is_empty() {
                continue;
            }
            update_preferred_match(
                &mut best_term_match,
                folded_match_range(body, &folded_body, term),
                term.chars().count(),
            );
        }
    }
    best_term_match.map(|(_, matched)| matched)
}

fn update_preferred_match(
    preferred: &mut Option<(usize, Range<usize>)>,
    candidate: Option<Range<usize>>,
    specificity: usize,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if preferred
        .as_ref()
        .is_none_or(|(current_specificity, current)| {
            specificity > *current_specificity
                || (specificity == *current_specificity && candidate.start < current.start)
        })
    {
        *preferred = Some((specificity, candidate));
    }
}

fn folded_match_range(body: &str, folded_body: &str, query_text: &str) -> Option<Range<usize>> {
    let folded_query = query_text.to_lowercase();
    if folded_query.is_empty() {
        return None;
    }
    let folded_start = folded_body.find(&folded_query)?;
    let folded_end = folded_start.saturating_add(folded_query.len());
    if body.is_ascii() {
        return Some(folded_start..folded_end);
    }
    original_range_for_folded_match(body, folded_start, folded_end)
}

fn original_range_for_folded_match(
    body: &str,
    folded_start: usize,
    folded_end: usize,
) -> Option<Range<usize>> {
    let mut folded_offset = 0_usize;
    let mut original_start = None;
    for (original_offset, character) in body.char_indices() {
        let folded_character_bytes = character.to_lowercase().map(char::len_utf8).sum::<usize>();
        let next_folded_offset = folded_offset.saturating_add(folded_character_bytes);
        if original_start.is_none() && folded_start < next_folded_offset {
            original_start = Some(original_offset);
        }
        if folded_end <= next_folded_offset {
            return original_start.map(|start| start..original_offset + character.len_utf8());
        }
        folded_offset = next_folded_offset;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_centers_the_actual_case_insensitive_match() {
        let body = format!("{}NeEdLe{}", "a".repeat(4_500), "z".repeat(4_500));
        let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

        assert!(truncated);
        assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
        let matched = snippet.find("NeEdLe").unwrap();
        assert_eq!(snippet[..matched].graphemes(true).count(), 157);
    }

    #[test]
    fn snippet_preserves_combining_and_emoji_graphemes() {
        let combining = "e\u{301}";
        let family = "👨‍👩‍👧‍👦";
        let body = format!("{}目标{}", combining.repeat(400), family.repeat(400));
        let (snippet, truncated) = search_snippet_fragment(&body, &["目标"]);
        let graphemes = snippet.graphemes(true).collect::<Vec<_>>();

        assert!(truncated);
        assert_eq!(graphemes.len(), SEARCH_SNIPPET_MAX_CHARS);
        assert_eq!(graphemes.first().copied(), Some(combining));
        assert_eq!(graphemes.last().copied(), Some(family));
        assert!(snippet.contains("目标"));
    }

    #[test]
    fn snippet_byte_bounds_pathological_grapheme_clusters() {
        let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
        let body = format!("{oversized_cluster}needle{oversized_cluster}");
        let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

        assert!(truncated);
        assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
        assert!(snippet.contains("needle"));
        assert_eq!(
            search_snippet_fragment(&oversized_cluster, &["x"]),
            (String::new(), true)
        );
    }

    #[test]
    fn snippet_handles_a_maximum_valid_core_body_without_offset_vectors() {
        let needle = "NeEdLe";
        let mut body = "x".repeat(MAX_CORE_CONTENT_BYTES - needle.len());
        body.push_str(needle);
        let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

        assert!(truncated);
        assert_eq!(snippet, format!("{}{}", "x".repeat(314), needle));
        assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
    }
}
