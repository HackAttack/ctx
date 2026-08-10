use std::{fs, io, path::Path};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};

use super::{
    paths::{bundled_hash, ensure_path_inside, sha256_hex},
    selection::{SkillAgentSelection, SkillSelectionSource},
    target::{resolve_targets_for_agents, SkillTarget},
    BUNDLED_SKILL_BODY, BUNDLED_SKILL_NAME, LEGACY_BUNDLED_SKILL_HASHES, METADATA_FILE,
};
use crate::filesystem::atomic_update;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
}

impl SkillInstallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug)]
pub struct StatusResult {
    pub target: SkillTarget,
    pub status: SkillInstallStatus,
    pub metadata: Option<SkillMetadata>,
    pub installed_hash: Option<String>,
}

#[derive(Debug)]
pub struct InstallResult {
    pub target: SkillTarget,
    pub success: bool,
    pub fatal: bool,
    pub previous_status: SkillInstallStatus,
    pub status: SkillInstallStatus,
    pub already_installed: bool,
    pub updated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub schema_version: u32,
    pub installer: String,
    pub skill_name: String,
    pub skill_hash: String,
    pub ctx_cli_version: String,
    pub installed_at: String,
}

impl SkillMetadata {
    pub fn current(product_version: &str) -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: bundled_hash(),
            ctx_cli_version: product_version.to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillInstallRequest {
    pub selection: SkillAgentSelection,
    pub project: bool,
    pub force: bool,
    pub product_version: String,
}

#[derive(Debug)]
pub struct SkillInstallReceipt {
    pub project: bool,
    pub selection: SkillAgentSelection,
    pub results: Vec<InstallResult>,
    pub fatal_failures: usize,
    pub already_installed: bool,
    pub updated: bool,
    pub modified_targets: usize,
}

#[derive(Debug, Clone)]
pub struct SkillStatusRequest {
    pub selection: SkillAgentSelection,
    pub project: bool,
}

#[derive(Debug)]
pub struct SkillStatusReceipt {
    pub project: bool,
    pub selection: SkillAgentSelection,
    pub results: Vec<StatusResult>,
    pub current_count: usize,
}

pub fn execute_install(
    request: SkillInstallRequest,
    context: &super::PathContext,
) -> Result<SkillInstallReceipt> {
    let targets = resolve_targets_for_agents(&request.selection.agents, request.project, context)?;
    let preserve_is_fatal = !matches!(
        request.selection.source,
        SkillSelectionSource::Detected | SkillSelectionSource::Fallback
    );
    let results = targets
        .iter()
        .map(|target| {
            install_target(
                target,
                request.force,
                preserve_is_fatal,
                &request.product_version,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SkillInstallReceipt {
        project: request.project,
        fatal_failures: results.iter().filter(|result| result.fatal).count(),
        already_installed: results.iter().all(|result| result.already_installed),
        updated: results.iter().any(|result| result.updated),
        modified_targets: results.iter().filter(|result| result.updated).count(),
        selection: request.selection,
        results,
    })
}

pub fn execute_status(
    request: SkillStatusRequest,
    context: &super::PathContext,
) -> Result<SkillStatusReceipt> {
    let targets = resolve_targets_for_agents(&request.selection.agents, request.project, context)?;
    let results = targets
        .iter()
        .map(status_target)
        .collect::<Result<Vec<_>>>()?;
    let current_count = results
        .iter()
        .filter(|result| result.status == SkillInstallStatus::Current)
        .count();
    Ok(SkillStatusReceipt {
        project: request.project,
        selection: request.selection,
        results,
        current_count,
    })
}

pub fn install_target(
    target: &SkillTarget,
    force: bool,
    modified_preserve_is_fatal: bool,
    product_version: &str,
) -> Result<InstallResult> {
    ensure_safe_skill_directory(target)?;
    let previous = status_target(target)?;
    let bundled_hash = bundled_hash();
    if previous.installed_hash.as_deref() == Some(bundled_hash.as_str()) {
        if !metadata_is_current(previous.metadata.as_ref(), product_version) {
            write_metadata(target, product_version)?;
        }
        return Ok(InstallResult {
            target: target.clone(),
            success: true,
            fatal: false,
            previous_status: previous.status,
            status: SkillInstallStatus::Current,
            already_installed: true,
            updated: false,
            error: None,
        });
    }
    if previous.status == SkillInstallStatus::Modified && !force {
        return Ok(InstallResult {
            target: target.clone(),
            success: false,
            fatal: modified_preserve_is_fatal,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some(format!(
                "preserved existing {} skill; use --force to replace",
                target.agent.display_name()
            )),
        });
    }
    write_skill_files(target, product_version)?;
    Ok(InstallResult {
        target: target.clone(),
        success: true,
        fatal: false,
        previous_status: previous.status,
        status: SkillInstallStatus::Current,
        already_installed: false,
        updated: matches!(
            previous.status,
            SkillInstallStatus::Stale | SkillInstallStatus::Modified
        ),
        error: None,
    })
}

pub fn status_target(target: &SkillTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    reject_symlink_directory(&target.skill_dir)?;
    let skill_file = target.skill_dir.join("SKILL.md");
    let metadata = read_metadata(&target.skill_dir);
    let installed_hash = match fs::read(&skill_file) {
        Ok(body) => Some(sha256_hex(&body)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", skill_file.display())),
    };
    let status = match installed_hash.as_deref() {
        None => SkillInstallStatus::Missing,
        Some(hash) if hash == bundled_hash() && metadata_manages_hash(metadata.as_ref(), hash) => {
            SkillInstallStatus::Current
        }
        Some(hash) if hash == bundled_hash() => SkillInstallStatus::Stale,
        Some(hash) if LEGACY_BUNDLED_SKILL_HASHES.contains(&hash) => SkillInstallStatus::Stale,
        Some(hash) => match metadata.as_ref() {
            Some(metadata) if metadata.skill_hash == hash => SkillInstallStatus::Stale,
            _ => SkillInstallStatus::Modified,
        },
    };
    Ok(StatusResult {
        target: target.clone(),
        status,
        metadata,
        installed_hash,
    })
}

fn ensure_safe_skill_directory(target: &SkillTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    reject_symlink_directory(&target.skill_dir)?;
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))
}

fn reject_symlink_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "refusing to install through symlink {}",
            path.display()
        )),
        Ok(metadata) if !metadata.is_dir() => Err(anyhow!(
            "skill target is not a directory: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn write_skill_files(target: &SkillTarget, product_version: &str) -> Result<()> {
    ensure_safe_skill_directory(target)?;
    atomic_update(&target.skill_dir.join("SKILL.md"), |_| {
        Ok(BUNDLED_SKILL_BODY.as_bytes().to_vec())
    })?;
    write_metadata(target, product_version)
}

fn write_metadata(target: &SkillTarget, product_version: &str) -> Result<()> {
    ensure_safe_skill_directory(target)?;
    let metadata = serde_json::to_vec_pretty(&SkillMetadata::current(product_version))?;
    atomic_update(&target.skill_dir.join(METADATA_FILE), |_| Ok(metadata))
}

fn read_metadata(skill_dir: &Path) -> Option<SkillMetadata> {
    let body = fs::read(skill_dir.join(METADATA_FILE)).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(metadata: Option<&SkillMetadata>, product_version: &str) -> bool {
    let hash = bundled_hash();
    metadata_manages_hash(metadata, &hash)
        && metadata.is_some_and(|metadata| metadata.ctx_cli_version == product_version)
}

fn metadata_manages_hash(metadata: Option<&SkillMetadata>, hash: &str) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.skill_name == BUNDLED_SKILL_NAME
            && metadata.skill_hash == hash
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_update_preserves_unrelated_skill_files() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, true, &context)
                .unwrap();
        fs::create_dir_all(&target.skill_dir).unwrap();
        fs::write(target.skill_dir.join("notes.txt"), "keep").unwrap();
        fs::write(target.skill_dir.join("SKILL.md"), "modified").unwrap();

        let result = install_target(&target, true, true, "1.0.0").unwrap();
        assert!(result.success);
        assert_eq!(
            fs::read_to_string(target.skill_dir.join("notes.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn interrupted_content_then_metadata_publication_is_stale_and_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, true, &context)
                .unwrap();
        fs::create_dir_all(target.skill_dir.join(METADATA_FILE)).unwrap();

        let error = install_target(&target, false, true, "1.0.0").unwrap_err();
        assert!(error.to_string().contains("non-regular file"));
        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Stale
        );

        fs::remove_dir(target.skill_dir.join(METADATA_FILE)).unwrap();
        let repaired = install_target(&target, false, true, "1.0.0").unwrap();
        assert!(repaired.already_installed);
        assert!(!repaired.updated);
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Current
        );
    }
}
