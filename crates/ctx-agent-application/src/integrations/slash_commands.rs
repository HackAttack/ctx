//! Slash-command installation orchestration independent of CLI rendering.

use anyhow::Result;
use ctx_agent_integrations::slash_commands::{
    execute_install, PathContext, SlashCommandAgent, SlashCommandInstallReceipt,
    SlashCommandInstallRequest, SlashCommandInstallResult, SlashCommandInstallStatus,
};

use crate::{IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity};

#[derive(Debug)]
pub struct SlashCommandInstallOutcome {
    pub receipt: SlashCommandInstallReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug, Clone)]
pub struct SlashCommandInstallApplicationRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
}

pub fn install(
    request: SlashCommandInstallApplicationRequest,
    context: &PathContext,
    identity: ProductIdentity<'_>,
) -> Result<SlashCommandInstallOutcome> {
    let receipt = execute_install(
        SlashCommandInstallRequest {
            agents: request.agents,
            all_agents: request.all_agents,
            project: request.project,
            force: request.force,
            product_version: identity.version.to_owned(),
        },
        context,
    )?;
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.results.len()),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        already_installed: Some(receipt.already_installed),
        updated: Some(receipt.updated),
        modified_targets: Some(receipt.modified_targets),
        ..IntegrationTelemetryFacts::default()
    };
    Ok(SlashCommandInstallOutcome { receipt, telemetry })
}

pub fn force_install_command(
    identity: ProductIdentity<'_>,
    result: &SlashCommandInstallResult,
) -> Option<String> {
    (result.status == SlashCommandInstallStatus::Modified).then(|| {
        format!(
            "{} integrations install slash-commands --agent {} --force",
            identity.name,
            result.agent.id()
        )
    })
}

#[cfg(test)]
mod tests;
