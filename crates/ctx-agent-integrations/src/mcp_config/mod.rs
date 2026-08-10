use std::{fs, io, path::Path};

use anyhow::{anyhow, Context, Result};

mod format;
mod registry;

pub use format::{server_command, status, upsert, ConfigKind, ConfigStatus, ServerCommand};
pub use registry::{
    parse_mcp_agent, project_detection_path, McpAgentArg, McpPathContext, McpScope, McpTarget,
};

use crate::filesystem::atomic_update;

const SERVER_NAME: &str = "ctx";
const SERVER_COMMAND: &str = "ctx";
const SERVER_ARGS: &[&str] = &["mcp", "serve"];

#[derive(Debug, Clone)]
pub struct McpInstallRequest {
    pub agents: Vec<McpAgentArg>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
}

#[derive(Debug)]
pub struct McpInstallReceipt {
    pub project: bool,
    pub selected_agents: usize,
    pub results: Vec<McpInstallResult>,
    pub failed: usize,
    pub modified: usize,
}

#[derive(Debug, Clone)]
pub struct McpStatusRequest {
    pub agents: Vec<McpAgentArg>,
    pub all_agents: bool,
    pub project: bool,
}

#[derive(Debug)]
pub struct McpStatusReceipt {
    pub request: McpStatusRequest,
    pub selected_agents: usize,
    pub results: Vec<McpStatusResult>,
}

#[derive(Debug)]
pub struct McpInstallResult {
    pub target: McpTarget,
    pub success: bool,
    pub previous_status: ConfigStatus,
    pub status: ConfigStatus,
    pub already_installed: bool,
    pub modified: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct McpStatusResult {
    pub target: McpTarget,
    pub status: ConfigStatus,
    pub error: Option<String>,
}

pub fn execute_install(request: McpInstallRequest, context: &McpPathContext) -> McpInstallReceipt {
    let agents = selected_agents(
        &request.agents,
        request.all_agents,
        request.project,
        context,
    );
    let results = agents
        .iter()
        .copied()
        .map(|agent| agent.target(request.project, context))
        .map(|target| install_target(&target, request.force))
        .collect::<Vec<_>>();
    McpInstallReceipt {
        project: request.project,
        selected_agents: agents.len(),
        failed: results.iter().filter(|result| !result.success).count(),
        modified: results.iter().filter(|result| result.modified).count(),
        results,
    }
}

pub fn execute_status(request: McpStatusRequest, context: &McpPathContext) -> McpStatusReceipt {
    let agents = selected_agents(
        &request.agents,
        request.all_agents,
        request.project,
        context,
    );
    McpStatusReceipt {
        selected_agents: agents.len(),
        results: agents
            .iter()
            .copied()
            .map(|agent| agent.target(request.project, context))
            .map(|target| status_target(&target))
            .collect(),
        request,
    }
}

fn selected_agents(
    agents: &[McpAgentArg],
    all_agents: bool,
    project: bool,
    context: &McpPathContext,
) -> Vec<McpAgentArg> {
    if all_agents {
        return if project {
            McpAgentArg::PROJECT_CAPABLE.to_vec()
        } else {
            McpAgentArg::ALL.to_vec()
        };
    }
    if !agents.is_empty() {
        return dedupe_agents(agents.iter().copied());
    }
    let candidates = if project {
        McpAgentArg::PROJECT_CAPABLE
    } else {
        McpAgentArg::ALL
    };
    candidates
        .iter()
        .copied()
        .filter(|agent| {
            if project {
                project_detection_path(*agent, context).exists()
            } else {
                agent.detected(context)
            }
        })
        .collect()
}

pub fn dedupe_agents(agents: impl IntoIterator<Item = McpAgentArg>) -> Vec<McpAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

pub fn install_target(target: &McpTarget, force: bool) -> McpInstallResult {
    let previous = status_target(target);
    if previous.status == ConfigStatus::Current {
        return McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: ConfigStatus::Current,
            already_installed: true,
            modified: false,
            error: None,
        };
    }
    if matches!(
        previous.status,
        ConfigStatus::Unsupported | ConfigStatus::Invalid
    ) {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: previous.error,
        };
    }
    if previous.status == ConfigStatus::Conflict && !force {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: Some(
                "existing ctx MCP server has different command or args; rerun with --force to overwrite"
                    .to_owned(),
            ),
        };
    }
    match write_target(target, force) {
        Ok(()) => McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: ConfigStatus::Current,
            already_installed: false,
            modified: true,
            error: None,
        },
        Err(error) => McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: ConfigStatus::Invalid,
            already_installed: false,
            modified: false,
            error: Some(error.to_string()),
        },
    }
}

pub fn status_target(target: &McpTarget) -> McpStatusResult {
    let (Some(path), Some(kind)) = (target.path.as_ref(), target.kind) else {
        return McpStatusResult {
            target: target.clone(),
            status: ConfigStatus::Unsupported,
            error: target.unsupported_reason.clone(),
        };
    };
    match read_target_status(path, kind) {
        Ok(status) => McpStatusResult {
            target: target.clone(),
            status,
            error: None,
        },
        Err(error) => McpStatusResult {
            target: target.clone(),
            status: ConfigStatus::Invalid,
            error: Some(error.to_string()),
        },
    }
}

fn read_target_status(path: &Path, kind: ConfigKind) -> Result<ConfigStatus> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ConfigStatus::Missing),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if body.trim().is_empty() {
        return Ok(ConfigStatus::Missing);
    }
    format::status(&body, kind, path)
}

fn write_target(target: &McpTarget, force: bool) -> Result<()> {
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    let kind = target
        .kind
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    atomic_update(path, |existing| {
        let existing = existing
            .map(|body| std::str::from_utf8(body).context("MCP config is not UTF-8"))
            .transpose()?
            .unwrap_or_default();
        Ok(format::upsert(existing, kind, force, path)?.into_bytes())
    })
    .with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn update_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(root.path().join("home"), root.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &context);
        let path = target.path.as_ref().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "{\"unrelated\":true}").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();

        let result = install_target(&target, false);
        assert!(result.success && result.modified);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
