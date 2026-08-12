use std::path::Path;

use anyhow::Result;
use ctx_upgrade_engine::{
    current_install_path, managed_install_marker_for_current_exe, path_diagnostics,
    read_state_json, ManagedInstallMarker, STATE_SCHEMA_VERSION,
};
use serde_json::json;

use crate::{config::AppConfig, ui::Ui};

pub(super) fn render_status(
    data_root: &Path,
    config: &AppConfig,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    let state = read_state_json().unwrap_or_else(|| {
        json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "never_checked"
        })
    });
    let current_version = super::super::ports::product_identity().version();
    let current_exe = current_install_path().ok();
    let path_diagnostics = current_exe
        .as_ref()
        .map(|path| path_diagnostics(path, current_version));
    let marker_result = managed_install_marker_for_current_exe();
    let valid_marker = match &marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => Some(marker),
        _ => None,
    };
    let state = ctx_cli_presentation::upgrade::reconcile_scheduled_state(state, valid_marker);
    let install = match marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => json!({
            "managed": true,
            "marker": "valid",
            "install_path": marker.install_path,
            "platform": marker.platform,
            "channel": marker.channel,
            "version": marker.version,
            "sha256": marker.sha256,
        }),
        Ok(ManagedInstallMarker::Absent) => json!({
            "managed": false,
            "marker": "absent",
            "reason": "ctx was not installed by the hosted installer"
        }),
        Ok(ManagedInstallMarker::Invalid { reason }) => json!({
            "managed": false,
            "marker": "corrupt",
            "reason": reason,
            "action": "reinstall ctx from https://ctx.rs/install",
        }),
        Err(error) => json!({
            "managed": false,
            "marker": "unavailable",
            "reason": format!("{error:#}"),
        }),
    };
    let path = path_diagnostics
        .as_ref()
        .map(ctx_cli_presentation::upgrade::path_diagnostics_json);
    let warnings = path_diagnostics
        .as_ref()
        .map(|diagnostics| diagnostics.warnings())
        .unwrap_or_default();
    let pro = crate::pro::lifecycle_status_json(data_root);
    let auto_mode = config.auto_upgrade_mode();
    ctx_cli_presentation::upgrade::render_status(
        ctx_cli_presentation::upgrade::UpgradeStatusView {
            current_version,
            auto_upgrade: auto_mode.as_str(),
            auto_enabled: config.auto_upgrade_enabled(),
            state: &state,
            install: &install,
            path: path.as_ref(),
            warnings,
            pro: &pro,
        },
        json_output,
        ui,
    )
}
