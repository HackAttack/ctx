//! Managed executable and path diagnostic facts.

use std::path::PathBuf;

use super::install::{managed_install_marker_for_current_exe, ManagedInstallMarker};

pub enum ManagedInstallDiagnostic {
    Absent,
    Invalid { reason: String },
    Valid { install_path: PathBuf },
    Unavailable { error: String },
}

pub struct UpgradeDiagnostics {
    pub install: ManagedInstallDiagnostic,
    pub findings: Vec<String>,
}

pub fn managed_install_executable() -> anyhow::Result<Option<PathBuf>> {
    Ok(match managed_install_marker_for_current_exe()? {
        ManagedInstallMarker::Valid(marker) => Some(marker.install_path),
        ManagedInstallMarker::Absent | ManagedInstallMarker::Invalid { .. } => None,
    })
}

pub fn upgrade_diagnostics() -> UpgradeDiagnostics {
    let mut findings = Vec::new();
    let install = match managed_install_marker_for_current_exe() {
        Ok(ManagedInstallMarker::Absent) => ManagedInstallDiagnostic::Absent,
        Ok(ManagedInstallMarker::Invalid { reason }) => {
            findings.push(format!(
                "managed ctx install marker is corrupt: {reason}; reinstall ctx from https://ctx.rs/install"
            ));
            ManagedInstallDiagnostic::Invalid { reason }
        }
        Ok(ManagedInstallMarker::Valid(marker)) => ManagedInstallDiagnostic::Valid {
            install_path: marker.install_path,
        },
        Err(error) => {
            findings.push(format!(
                "could not inspect the running ctx executable for managed upgrades: {error:#}"
            ));
            ManagedInstallDiagnostic::Unavailable {
                error: format!("{error:#}"),
            }
        }
    };
    UpgradeDiagnostics { install, findings }
}
