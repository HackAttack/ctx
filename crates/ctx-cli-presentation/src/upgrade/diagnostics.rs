use ctx_upgrade_engine::{ManagedInstallDiagnostic, UpgradeDiagnostics as EngineDiagnostics};
use serde_json::{json, Value};

use super::path_diagnostics_json;

pub struct UpgradeDiagnostics {
    pub report: Value,
    pub findings: Vec<String>,
}

pub fn present_upgrade_diagnostics(
    auto_mode: &str,
    auto_enabled: bool,
    diagnostics: EngineDiagnostics,
) -> UpgradeDiagnostics {
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
            "path": path_diagnostics_json(&path),
        }),
        ManagedInstallDiagnostic::Unavailable { error } => json!({
            "managed": false,
            "marker": "unavailable",
            "error": error,
        }),
    };
    UpgradeDiagnostics {
        report: json!({
            "auto": auto_mode,
            "auto_enabled": auto_enabled,
            "install": install,
        }),
        findings: diagnostics.findings,
    }
}
