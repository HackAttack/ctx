use ctx_upgrade_engine::ManagedInstallDiagnostic;
use serde_json::{json, Value};

use crate::config::AppConfig;

pub(crate) struct UpgradeDiagnostics {
    pub(crate) report: Value,
    pub(crate) findings: Vec<String>,
}

pub(crate) fn upgrade_diagnostics(config: &AppConfig) -> UpgradeDiagnostics {
    let mode = config.auto_upgrade_mode();
    let diagnostics = ctx_upgrade_engine::upgrade_diagnostics(super::ports::product_identity());
    let install = match diagnostics.install {
        ManagedInstallDiagnostic::Absent => json!({
            "managed": false,
            "marker": "absent",
        }),
        ManagedInstallDiagnostic::Invalid { reason } => json!({
            "managed": false,
            "marker": "corrupt",
            "error": reason,
        }),
        ManagedInstallDiagnostic::Valid { install_path, path } => json!({
            "managed": true,
            "marker": "valid",
            "install_path": install_path,
            "path": super::presentation::path_diagnostics_json(&path),
        }),
        ManagedInstallDiagnostic::Unavailable { error } => json!({
            "managed": false,
            "marker": "unavailable",
            "error": error,
        }),
    };
    UpgradeDiagnostics {
        report: json!({
            "auto": mode.as_str(),
            "auto_enabled": mode.enabled(),
            "install": install,
        }),
        findings: diagnostics.findings,
    }
}
