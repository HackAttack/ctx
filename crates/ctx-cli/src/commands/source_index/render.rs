use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{managed_data_root, utc_now};
use ctx_history_index::{EventSearchFilters, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    commands::mcp_tool_call::{
        append_mcp_tool_call_markdown, append_mcp_tool_call_text, MCP_TOOL_CALL_JSON_GUIDANCE,
    },
    output::{compact_json, OutputFormat},
    presentation_limit::{
        enforce_presentation_cli_output_limit, enforce_presentation_output_limit,
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    transcript::{shell_quote_arg, write_output},
};

use super::search::{
    semantic_reason_code, NormalizedSearchQuery, SearchCollection, SearchHit, SearchPresentation,
    SourceSearchRequest,
};
use crate::RefreshArg;

mod human;
mod locate;
mod search;
mod show;

pub(super) use locate::render_locate_document;
pub(super) use search::{render_search_document, render_search_not_ready_document};
pub(super) use show::render_show_document;

pub(in crate::commands::source_index) use ctx_history_query::SEARCH_SNIPPET_MAX_CHARS;
#[cfg(test)]
pub(in crate::commands::source_index) use ctx_history_query::{
    search_snippet_fragment, SEARCH_SNIPPET_MAX_BYTES,
};

pub(super) fn pretty_json_stdout_bytes(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len().saturating_add(1))
}

pub(super) fn stdout_body_bytes(body: &str) -> usize {
    body.len()
        .saturating_add(usize::from(!body.ends_with('\n')))
}

struct SearchJsonInput<'input> {
    request: &'input SourceSearchRequest,
    data_root: &'input Path,
    index: &'input VerifiedIndex,
    collection: &'input SearchCollection,
    filters: &'input EventSearchFilters,
    presentations: &'input [SearchPresentation],
    copied_lineages: &'input [Value],
    refresh_mode: RefreshArg,
    metrics: SearchRenderMetrics<'input>,
}

struct SearchRenderMetrics<'a> {
    refresh_status: &'a str,
    refresh_source_count: usize,
    query_duration: Duration,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn search_json(
    request: &SourceSearchRequest,
    data_root: &Path,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    presentations: &[SearchPresentation],
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    let copied_lineages = (0..collection.result_window.hits.len())
        .map(|_| {
            json!({
                "schema_version": 1,
                "observed_count": 0,
                "returned": 0,
                "occurrences": [],
                "relationship_counts": {},
                "truncated": false,
            })
        })
        .collect::<Vec<_>>();
    search_json_with_lineages(
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        &copied_lineages,
        RefreshArg::Off,
        refresh_status,
        refresh_source_count,
        query_duration,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_json_with_lineages(
    request: &SourceSearchRequest,
    data_root: &Path,
    index: &VerifiedIndex,
    collection: &SearchCollection,
    filters: &EventSearchFilters,
    presentations: &[SearchPresentation],
    copied_lineages: &[Value],
    refresh_mode: RefreshArg,
    refresh_status: &str,
    refresh_source_count: usize,
    query_duration: Duration,
) -> Result<Value> {
    render_search_json(SearchJsonInput {
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        copied_lineages,
        refresh_mode,
        metrics: SearchRenderMetrics {
            refresh_status,
            refresh_source_count,
            query_duration,
        },
    })
}

fn render_search_json(input: SearchJsonInput<'_>) -> Result<Value> {
    let SearchJsonInput {
        request,
        data_root,
        index,
        collection,
        filters,
        presentations,
        copied_lineages,
        refresh_mode,
        metrics,
    } = input;
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let result_scope = if request.events { "event" } else { "session" };
    let command_prefix = follow_up_command_prefix(data_root);
    if presentations.len() != collection.result_window.hits.len() {
        return Err(anyhow!(
            "pinned Core lookup returned {} search presentations for {} hits",
            presentations.len(),
            collection.result_window.hits.len()
        ));
    }
    if copied_lineages.len() != collection.result_window.hits.len() {
        return Err(anyhow!(
            "pinned Core lookup returned {} copied-lineage values for {} hits",
            copied_lineages.len(),
            collection.result_window.hits.len()
        ));
    }
    let results = collection
        .result_window
        .hits
        .iter()
        .zip(presentations)
        .zip(copied_lineages)
        .enumerate()
        .map(|(offset, ((hit, presentation), copied_lineage))| {
            if presentation.event_id != hit.event.event_id {
                return Err(anyhow!(
                    "out-of-order search presentation for event {}",
                    presentation.event_id
                ));
            }
            search_result_json(
                hit,
                presentation,
                result_scope,
                &normalized_query,
                offset.saturating_add(1),
                &command_prefix,
                copied_lineage,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let phase_attribution = phase_attribution(metrics.query_duration);
    let semantic_diagnostics = external_semantic_diagnostics(collection);
    Ok(compact_json(json!({
        "schema_version": 1,
        "payload_type": "search_results",
        "query": normalized_query.display(),
        "filters": {
            "provider": filters.provider,
            "history_source": filters.history_source,
            "provider_key": filters.provider_key,
            "source_id": filters.source_id,
            "source_format": filters.source_format,
            "workspace": request.workspace,
            "since": request.since,
            "content_scope": filters.content_scope.as_str(),
            "event_type": request.event_type,
            "file": request.file.as_ref().map(|path| path.display().to_string()),
            "session": request.session,
            "primary_only": request.primary_only.then_some(true),
            "include_subagents": request.include_subagents.then_some(true),
            "include_current_session": request.include_current_session.then_some(true),
        },
        "freshness": {
            "mode": refresh_mode.as_str(),
            "status": metrics.refresh_status,
            "source_count": metrics.refresh_source_count,
        },
        "retrieval": {
            "requested_mode": collection.requested_backend.as_str(),
            "effective_mode": collection.effective_backend.as_str(),
            "semantic_weight": collection.semantic_weight,
            "semantic_status": collection.semantic_status,
            "semantic_fallback_code": collection.semantic_fallback.as_ref().map(semantic_fallback_code),
            "semantic_fallback": collection.semantic_fallback.as_ref().map(|fallback| semantic_fallback_detail(fallback.reason, &fallback.detail)),
            "semantic_diagnostics": semantic_diagnostics,
            "index": "core",
            "generation_id": index.generation_id(),
            "indexed_documents": index.document_count(),
            "phase_attribution": phase_attribution,
        },
        "phase_attribution": phase_attribution,
        "generated_at": utc_now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "results": results,
        "result_window": {
            "limit": collection.result_window.limit,
            "returned": results.len(),
            "more_available": collection.result_window.more_available,
        },
        "truncation": {
            "candidate_pool": collection.candidate_pool,
            "candidate_pool_truncated": collection.candidate_pool_truncated,
        },
    })))
}

fn semantic_fallback_code(
    fallback: &ctx_history_query::SemanticFallbackDiagnostics,
) -> &'static str {
    fallback
        .reason
        .map(semantic_reason_code)
        .unwrap_or("semantic_query_failed")
}

fn external_semantic_diagnostics(collection: &SearchCollection) -> Option<Value> {
    let mut diagnostics = collection.semantic_diagnostics.clone()?;
    let Some(fallback) = collection.semantic_fallback.as_ref() else {
        return Some(diagnostics);
    };
    let Some(object) = diagnostics.as_object_mut() else {
        return Some(diagnostics);
    };
    object.insert(
        "fallback".to_owned(),
        json!({
            "code": semantic_fallback_code(fallback),
            "detail": semantic_fallback_detail(fallback.reason, &fallback.detail),
        }),
    );
    Some(diagnostics)
}

fn search_result_json(
    hit: &SearchHit,
    presentation: &SearchPresentation,
    result_scope: &str,
    query: &NormalizedSearchQuery,
    rank: usize,
    command_prefix: &str,
    copied_lineage: &Value,
) -> Result<Value> {
    let (snippet, snippet_truncated) = search_snippet(presentation);
    let event = &hit.event;
    let event_id = event.event_id;
    let session_id = event.session_id;
    let item_id = if result_scope == "session" {
        session_id
    } else {
        event_id
    };
    let title = match event.role.as_deref() {
        Some(role) => format!("{} {role} {}", event.provider, event.event_type),
        None => format!("{} {}", event.provider, event.event_type),
    };
    let mut next = vec![format!(
        "{command_prefix} show event {event_id} --window 10"
    )];
    if result_scope == "session" {
        next.insert(0, format!("{command_prefix} show session {session_id}"));
    }
    let query_arguments = search_query_command_arguments(query);
    if !query_arguments.is_empty() {
        next.push(format!(
            "{command_prefix} search {query_arguments} --session {session_id}"
        ));
    }
    Ok(compact_json(json!({
        "item_id": item_id,
        "result_type": if result_scope == "session" { "session_result" } else { "event" },
        "ctx_event_id": event_id,
        "ctx_session_id": session_id,
        "session_id": session_id,
        "event_id": event_id,
        "event_seq": event.event_sequence,
        "title": title,
        "snippet": snippet,
        "snippet_truncated": snippet_truncated,
        "snippet_max_chars": SEARCH_SNIPPET_MAX_CHARS,
        "rank": rank,
        "retrieval_score": hit.score,
        "result_scope": result_scope,
        "session_importance": (result_scope == "session").then_some(hit.score),
        "more_matches_in_session": (result_scope == "session")
            .then_some(hit.more_matches_in_session),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "parent_ctx_session_id": event.parent_session_id,
        "root_ctx_session_id": event.root_session_id,
        "session_relationship": event.session_relationship,
        "event_origin": super::event_origin_json(&event.event_origin),
        "copied_lineage": copied_lineage,
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "timestamp": timestamp_json(event.occurred_at_unix_ms),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "suggested_next_commands": next,
        "citations": [{
            "item_id": event_id,
            "target_type": "event",
            "ctx_event_id": event_id,
            "ctx_session_id": session_id,
            "provider": event.provider,
            "session_id": session_id,
            "event_seq": event.event_sequence,
        }],
        "visibility": "local",
    })))
}

fn search_snippet(presentation: &SearchPresentation) -> (&str, bool) {
    (
        presentation.snippet.as_str(),
        presentation.snippet_truncated,
    )
}

fn semantic_fallback_detail(
    reason: Option<ctx_history_query::SemanticReason>,
    detail: &str,
) -> String {
    match reason {
        Some(ctx_history_query::SemanticReason::PolicyDisabled) => {
            "local semantic retrieval is disabled".to_owned()
        }
        Some(ctx_history_query::SemanticReason::ExecutionUnavailable) => {
            "local semantic retrieval is unavailable because the ctx daemon is disabled".to_owned()
        }
        Some(ctx_history_query::SemanticReason::ContentScopeUnsupported) => {
            format!("{detail}; use --backend lexical or choose --content-scope all|transcript")
        }
        Some(ctx_history_query::SemanticReason::EventTypeUnsupported) => {
            format!("{detail}; use --backend lexical or remove --event-type")
        }
        _ => detail.to_owned(),
    }
}

fn search_query_command_arguments(query: &NormalizedSearchQuery) -> String {
    let mut arguments = Vec::new();
    if let Some(positional) = query.positional() {
        arguments.push(shell_quote_arg(positional));
    }
    for term in query.terms() {
        arguments.push(format!("--term={}", shell_quote_arg(term)));
    }
    arguments.join(" ")
}

pub(super) fn follow_up_command_prefix(data_root: &Path) -> String {
    if managed_data_root().is_ok_and(|default_root| default_root == data_root) {
        return "ctx".to_owned();
    }
    let data_root = data_root.to_string_lossy();
    format!("ctx --data-root {}", shell_quote_arg(data_root.as_ref()))
}

pub(super) fn write_show_value(
    value: Value,
    format: OutputFormat,
    out: Option<PathBuf>,
    event_id: Uuid,
) -> Result<usize> {
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&value)?,
        OutputFormat::Jsonl => render_show_jsonl(&value)?,
        OutputFormat::Text => render_show_text(&value),
        OutputFormat::Markdown => render_show_markdown(&value),
    };
    enforce_presentation_cli_output_limit(
        &body,
        out.is_none(),
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
        event_id,
    )?;
    let output_bytes = if out.is_some() {
        body.len()
    } else {
        stdout_body_bytes(&body)
    };
    write_output(body, out).map(|()| output_bytes)
}

fn render_show_jsonl(value: &Value) -> Result<String> {
    let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let lines = events
        .iter()
        .map(|event| {
            if value["target"] == "session" {
                serde_json::to_string(&compact_json(json!({
                    "schema_version": 1,
                    "payload_type": "session_transcript_event",
                    "mode": value["mode"],
                    "ctx_session_id": value["ctx_session_id"],
                    "provider": value["provider"],
                    "provider_session_id": value["provider_session_id"],
                    "event": event,
                })))
            } else {
                serde_json::to_string(event)
            }
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(lines.join("\n") + "\n")
    }
}

pub(super) fn enforce_json_output_limit(
    value: &Value,
    output_limit_bytes: usize,
    event_id: Uuid,
) -> Result<()> {
    let serialized_bytes = serde_json::to_vec(value)?.len();
    enforce_presentation_output_limit(serialized_bytes, output_limit_bytes, event_id)?;
    Ok(())
}

fn render_show_text(value: &Value) -> String {
    let mut output = String::new();
    match value["target"].as_str() {
        Some("session") => {
            output.push_str(&format!(
                "ctx_session_id: {}\nprovider: {}\n",
                value["ctx_session_id"].as_str().unwrap_or("unknown"),
                value["provider"].as_str().unwrap_or("unknown")
            ));
            if let Some(provider_session_id) = value["provider_session_id"].as_str() {
                output.push_str(&format!("provider_session_id: {provider_session_id}\n"));
            }
            output.push_str(&format!(
                "mode: {}\nformat: text\n\n",
                value["mode"].as_str().unwrap_or("lite")
            ));
        }
        _ => {
            output.push_str(&format!(
                "ctx_event_id: {}\nctx_session_id: {}\n\n",
                value["ctx_event_id"].as_str().unwrap_or("unknown"),
                value["ctx_session_id"].as_str().unwrap_or("unknown")
            ));
        }
    }
    append_copied_lineage_text(&mut output, value);
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "[{}] {} {} {}\n",
            event["occurred_at"].as_str().unwrap_or("-"),
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
        ));
        append_mcp_tool_call_text(&mut output, event, "", MCP_TOOL_CALL_JSON_GUIDANCE);
        output.push_str(event["text"].as_str().unwrap_or_default());
        output.push_str("\n\n");
    }
    output
}

fn render_show_markdown(value: &Value) -> String {
    let mut output = match value["target"].as_str() {
        Some("session") => format!(
            "# {} session {}\n\n- ctx_session_id: `{}`\n",
            value["provider"].as_str().unwrap_or("unknown"),
            value["provider_session_id"]
                .as_str()
                .or_else(|| value["ctx_session_id"].as_str())
                .unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown")
        ),
        _ => format!(
            "# Event {}\n\n- ctx_session_id: `{}`\n",
            value["ctx_event_id"].as_str().unwrap_or("unknown"),
            value["ctx_session_id"].as_str().unwrap_or("unknown")
        ),
    };
    append_copied_lineage_markdown(&mut output, value);
    for event in value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let role = event["role"]
            .as_str()
            .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
        output.push_str(&format!(
            "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n",
            role,
            event["event_type"].as_str().unwrap_or("event"),
            event["occurred_at"].as_str().unwrap_or("-"),
            event["ctx_event_id"].as_str().unwrap_or("unknown"),
        ));
        if append_mcp_tool_call_markdown(&mut output, event) {
            output.push('\n');
        }
        output.push_str(event["text"].as_str().unwrap_or_default());
        output.push('\n');
    }
    output
}

fn append_copied_lineage_text(output: &mut String, value: &Value) {
    let Some((lineage, observed, resolution, selected_depth)) =
        super::copied_lineage::copied_lineage_summary(value)
    else {
        return;
    };
    if observed == 0 && resolution.is_none_or(|state| state == "resolved") && selected_depth == 0 {
        return;
    }
    if let Some(resolution) = resolution {
        output.push_str(&format!(
            "lineage_resolution: {resolution} selected_depth={selected_depth}\n"
        ));
    }
    let truncated = lineage["truncated"].as_bool().unwrap_or(true);
    let summary = if truncated {
        format!("copied_to: at least {observed} sessions\n")
    } else {
        format!("copied_to: {observed} sessions\n")
    };
    output.push_str(&summary);
    let command_prefix = value["_command_prefix"].as_str().unwrap_or("ctx");
    for occurrence in lineage["occurrences"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(20)
    {
        let session = occurrence["ctx_session_id"].as_str().unwrap_or("unknown");
        let event = occurrence["ctx_event_id"].as_str().unwrap_or("unknown");
        let relationship = occurrence["session_relationship"]
            .as_str()
            .unwrap_or("inherited");
        let depth = occurrence["depth"].as_u64().unwrap_or(0);
        output.push_str(&format!(
            "inherited: session={session} event={event} relationship={relationship} depth={depth}\n"
        ));
        output.push_str(&format!("next: {command_prefix} show session {session}\n"));
    }
    output.push('\n');
}

fn append_copied_lineage_markdown(output: &mut String, value: &Value) {
    let Some((lineage, observed, resolution, selected_depth)) =
        super::copied_lineage::copied_lineage_summary(value)
    else {
        return;
    };
    if observed == 0 && resolution.is_none_or(|state| state == "resolved") && selected_depth == 0 {
        return;
    }
    if let Some(resolution) = resolution {
        output.push_str(&format!(
            "\n## Copied lineage\n\nResolution: `{resolution}` at selected depth {selected_depth}.\n"
        ));
    }
    if observed == 0 {
        return;
    }
    let truncated = lineage["truncated"].as_bool().unwrap_or(true);
    let count = if truncated {
        format!("at least {observed}")
    } else {
        observed.to_string()
    };
    output.push_str(&format!("\n### Inherited by {count} sessions\n"));
    let command_prefix = value["_command_prefix"].as_str().unwrap_or("ctx");
    for occurrence in lineage["occurrences"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(20)
    {
        let session = occurrence["ctx_session_id"].as_str().unwrap_or("unknown");
        let event = occurrence["ctx_event_id"].as_str().unwrap_or("unknown");
        let relationship = occurrence["session_relationship"]
            .as_str()
            .unwrap_or("inherited");
        let depth = occurrence["depth"].as_u64().unwrap_or(0);
        output.push_str(&format!(
            "\n- `{relationship}` session `{session}`, event `{event}`, depth {depth}\n"
        ));
        output.push_str(&format!("  - `{command_prefix} show session {session}`\n"));
    }
}

pub(super) fn timestamp_json(timestamp: Option<i64>) -> Option<String> {
    timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn phase_attribution(query: Duration) -> Value {
    json!({
        "discovery_seconds": 0.0,
        "writer_open_seconds": 0.0,
        "scan_and_stage_seconds": 0.0,
        "scanner_worker_busy_seconds": 0.0,
        "writer_add_document_seconds": 0.0,
        "certification_seconds": 0.0,
        "index_commit_seconds": 0.0,
        "refresh_total_seconds": 0.0,
        "query_seconds": query.as_secs_f64(),
        "catalog_sources": 0,
        "catalog_source_bytes": 0,
        "cold_sources": 0,
        "appended_sources": 0,
        "replaced_sources": 0,
        "replayed_sources": 0,
        "deleted_sources": 0,
        "scanner_bytes_read": 0,
        "checkpoint_validation_bytes": 0,
        "scanner_workers": 0,
        "complete_records_scanned": 0,
        "retained_records_scanned": 0,
        "rejected_records_scanned": 0,
        "ignored_records_scanned": 0,
        "staged_documents": 0,
    })
}

#[cfg(test)]
mod tests;
