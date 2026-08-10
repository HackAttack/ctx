//! Managed executable and path diagnostic facts.

use std::path::PathBuf;

use super::{
    install::{managed_install_marker_for_current_exe, ManagedInstallMarker},
    path::path_diagnostics,
    PathDiagnostics, ProductBuildIdentity,
};

pub enum ManagedInstallDiagnostic {
    Absent,
    Invalid {
        reason: String,
    },
    Valid {
        install_path: PathBuf,
        path: PathDiagnostics,
    },
    Unavailable {
        error: String,
    },
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

pub fn upgrade_diagnostics(identity: ProductBuildIdentity) -> UpgradeDiagnostics {
    let mut findings = Vec::new();
    let install = match managed_install_marker_for_current_exe() {
        Ok(ManagedInstallMarker::Absent) => ManagedInstallDiagnostic::Absent,
        Ok(ManagedInstallMarker::Invalid { reason }) => {
            findings.push(format!(
                "managed ctx install marker is corrupt: {reason}; reinstall ctx from https://ctx.rs/install"
            ));
            ManagedInstallDiagnostic::Invalid { reason }
        }
        Ok(ManagedInstallMarker::Valid(marker)) => {
            let path = path_diagnostics(&marker.install_path, identity.version());
            if let Some(reason) = path.background_apply_block_reason() {
                findings.push(format!(
                    "background ctx upgrade is blocked ({}): {}",
                    reason.code(),
                    reason.action()
                ));
            }
            ManagedInstallDiagnostic::Valid {
                install_path: marker.install_path,
                path,
            }
        }
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
