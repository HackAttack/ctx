use ctx_upgrade_engine::{
    invalid_install_marker_recovery_guidance, unmanaged_install_conversion_guidance,
    ManagedInstallDiagnostic, UpgradeDiagnostics as EngineDiagnostics,
};
use serde_json::{json, Value};

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
            "reason": "ctx was not installed by the hosted installer",
            "action": unmanaged_install_conversion_guidance(),
        }),
        ManagedInstallDiagnostic::Invalid { reason } => json!({
            "managed": false,
            "marker": "corrupt",
            "error": reason,
            "action": invalid_install_marker_recovery_guidance(),
        }),
        ManagedInstallDiagnostic::Valid { install_path } => json!({
            "managed": true,
            "marker": "valid",
            "install_path": install_path,
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
