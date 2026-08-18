use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::{
    analytics::IndexTelemetry,
    config::{self, CONFIG_FILE},
    output::compact_json,
    semantic::source_epoch_status_report,
    ui::Ui,
};

#[cfg(test)]
pub(crate) mod dashboard_fixture;

pub(crate) use ctx_cli_presentation::commands::index::IndexArgs;

struct CliIndexReadiness;

struct CliIndexMode;

impl ctx_cli_presentation::commands::index::IndexReadinessPort for CliIndexReadiness {
    fn snapshot(&mut self, data_root: &Path) -> Result<Value> {
        index_readiness_snapshot(data_root)
    }
}

impl ctx_cli_presentation::commands::index::IndexModePort for CliIndexMode {
    fn current(&mut self, data_root: &Path) -> Result<Value> {
        let config = config::AppConfig::load(data_root)?;
        Ok(index_mode_report(
            data_root,
            config.indexing.mode.as_str(),
            None,
            None,
        ))
    }

    fn update(
        &mut self,
        data_root: &Path,
        mode: ctx_cli_presentation::commands::index::IndexModeArg,
    ) -> Result<Value> {
        let config = config::AppConfig::load(data_root)?;
        let update = crate::semantic::update_indexing_mode(data_root, &config, mode.is_auto())?;
        let applied_mode = if update.automatic { "auto" } else { "manual" };
        Ok(index_mode_report(
            data_root,
            applied_mode,
            Some(mode.as_str()),
            Some(update),
        ))
    }
}

pub(crate) fn run_index(
    args: IndexArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut IndexTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    ctx_cli_presentation::commands::index::run_index(
        args,
        data_root,
        quiet,
        telemetry,
        &mut CliIndexReadiness,
        &mut CliIndexMode,
        ui,
    )
}

fn index_mode_report(
    data_root: &Path,
    mode: &str,
    requested_mode: Option<&str>,
    update: Option<ctx_daemon_cli::IndexingModeUpdate>,
) -> Value {
    let mut report = json!({
        "schema_version": 1,
        "indexing": {
            "mode": mode,
        },
        "config_path": data_root.join(CONFIG_FILE),
        "local_only": true,
        "read_only": update.is_none(),
    });
    if let Some(requested_mode) = requested_mode {
        report["indexing"]["requested_mode"] = json!(requested_mode);
        report["indexing"]["overridden"] = json!(requested_mode != mode);
    }
    if let Some(update) = update {
        report["daemon"] = json!({
            "running": update.running,
            "pid": update.pid,
            "persistent": update.persistent,
            "supervisor": update.supervisor,
        });
    }
    compact_json(report)
}

fn index_readiness_snapshot(data_root: &Path) -> Result<Value> {
    let config = config::AppConfig::load(data_root)?;
    let source = source_epoch_status_report(data_root, &config)?;
    let source_lexical = &source.report["lexical"];
    let source_semantic = &source.report["semantic"];
    let semantic_flat = &source_semantic["flat_f32"];
    let source_daemon = &source.report["daemon"];
    Ok(compact_json(json!({
        "schema_version": 1,
        "initialized": source.initialized,
        "indexing": {
            "mode": config.indexing.mode.as_str(),
        },
        "lexical": {
            "status": source_lexical.get("status"),
            "reason": source_lexical.get("reason"),
            "generation_id": source_lexical.get("generation_id"),
            "indexed_items": source.indexed_items,
            "indexed_sessions": source.indexed_sessions,
            "indexed_events": source.indexed_events,
            "indexed_sources": source.indexed_sources,
            "certified_source_bytes": source_lexical.get("certified_source_bytes"),
        },
        "refresh": {
            "status": source.report["refresh"].get("status"),
            "reason": source.report["refresh"].get("reason"),
            "request_state": source.report["refresh"].get("request_state"),
            "request_id": source.report["refresh"].get("request_id"),
            "logical_request_id": source.report["refresh"].get("logical_request_id"),
            "logical_phase": source.report["refresh"].get("logical_phase"),
            "physical_attempt_id": source.report["refresh"].get("physical_attempt_id"),
            "physical_attempt_state": source.report["refresh"].get("physical_attempt_state"),
            "progress_owner_request_id": source.report["refresh"].get("progress_owner_request_id"),
            "progress_owner_attempt_state": source.report["refresh"].get("progress_owner_attempt_state"),
            "structured_outcome": source.report["refresh"].get("structured_outcome"),
            "published_generation": source.report["refresh"].get("published_generation"),
            "generation_id": source.report["refresh"].get("generation_id"),
            "generation_matches": source.report["refresh"].get("generation_matches"),
            "certified_source_count": source.report["refresh"].get("certified_source_count"),
            "certified_source_bytes": source.report["refresh"].get("certified_source_bytes"),
            "progress": source.report["refresh"].get("progress"),
        },
        "semantic": {
            "status": source_semantic.get("status"),
            "reason": source_semantic.get("reason"),
            "enabled": source_semantic.get("enabled"),
            "coverage": {
                "searchable_items": semantic_flat.get("semantic_documents"),
                "embedded_items": semantic_flat.get("active_events"),
                "embedded_chunks": semantic_flat.get("active_chunks"),
            },
        },
        "daemon": {
            "status": source_daemon.get("status"),
            "running": source_daemon.get("running"),
            "jobs": {
                "semantic_index": source_daemon.get("jobs").and_then(|jobs| jobs.get("semantic_index")),
            },
        },
        "local_only": true,
        "read_only": true,
    })))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn readiness_port_preserves_exact_engine_logical_and_physical_status() {
        crate::semantic::initialize().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let status_path = data_root.join("daemon/jobs/core-refresh.json");
        fs::create_dir_all(status_path.parent().unwrap()).unwrap();
        let structured_outcome = json!({
            "code": "source_refresh_failed",
            "class": "internal",
            "retryable": false,
            "affected_routes": [],
            "retryable_routes": [],
            "blocked_routes": [],
            "physical_attempt_id": "physical-attempt",
        });
        fs::write(
            &status_path,
            serde_json::to_vec(&json!({
                "request_id": "logical-request",
                "request_state": "failed",
                "logical_request_id": "logical-request",
                "logical_phase": "terminal",
                "physical_attempt_id": "physical-attempt",
                "physical_attempt_state": "failed",
                "progress_owner_request_id": "progress-owner",
                "progress_owner_attempt_state": "failed",
                "structured_outcome": structured_outcome,
                "progress": {
                    "phase": "committed",
                    "completed_sources": 0,
                    "total_sources": 0,
                    "total_sources_known": true,
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let snapshot = index_readiness_snapshot(&data_root).unwrap();
        assert_eq!(snapshot["refresh"]["logical_request_id"], "logical-request");
        assert_eq!(snapshot["refresh"]["logical_phase"], "terminal");
        assert_eq!(
            snapshot["refresh"]["physical_attempt_id"],
            "physical-attempt"
        );
        assert_eq!(snapshot["refresh"]["physical_attempt_state"], "failed");
        assert_eq!(
            snapshot["refresh"]["progress_owner_request_id"],
            "progress-owner"
        );
        assert_eq!(
            snapshot["refresh"]["progress_owner_attempt_state"],
            "failed"
        );
        assert_eq!(
            snapshot["refresh"]["structured_outcome"],
            structured_outcome
        );
    }
}
