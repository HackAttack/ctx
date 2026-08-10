use ctx_upgrade_engine::{PathDiagnostics, PathResolverStatus};
use serde_json::{json, Value};

pub(super) fn path_diagnostics_json(diagnostics: &PathDiagnostics) -> Value {
    let resolver_status = diagnostics.resolver_status();
    let block_reason = diagnostics.background_apply_block_reason();
    json!({
        "current_exe": diagnostics.current_exe().display().to_string(),
        "first_ctx": diagnostics
            .entries()
            .first()
            .map(|entry| entry.path().display().to_string()),
        "resolver_status": resolver_status.code(),
        "managed_executable_wins": resolver_status == PathResolverStatus::ManagedExecutableWins,
        "background_apply": {
            "allowed": block_reason.is_none(),
            "reason": block_reason.map(ctx_upgrade_engine::BackgroundApplyBlockReason::code),
            "action": block_reason.map(ctx_upgrade_engine::BackgroundApplyBlockReason::action),
        },
        "entries": diagnostics.entries().iter().map(|entry| {
            json!({
                "path": entry.path().display().to_string(),
                "version": entry.version(),
                "current": entry.current(),
            })
        }).collect::<Vec<_>>(),
        "warnings": diagnostics.warnings(),
    })
}
