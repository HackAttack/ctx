use anyhow::Result;
use clap::Args;

use crate::analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection};
use crate::output::JsonOutputFormat;
use crate::ui::Ui;

mod install;
mod selection;

mod agents {
    pub(super) use ctx_agent_integrations::skill::{picker_agents, SkillAgentArg};
}

mod paths {
    #[cfg(test)]
    pub(super) use ctx_agent_integrations::skill::ensure_path_inside;
    pub(super) use ctx_agent_integrations::skill::PathContext;
}

mod target {
    pub(super) use ctx_agent_integrations::skill::single_target;
}

#[cfg(test)]
mod tests;

use agents::SkillAgentArg;
use install::{run_install, run_status};
use paths::PathContext;

use ctx_agent_integrations::skill::BUNDLED_SKILL_NAME;
#[cfg(test)]
const METADATA_FILE: &str = ".ctx-skill.json";

#[derive(Debug, Args)]
pub(crate) struct SkillInstallArgs {
    #[arg(
        long = "agent",
        value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents"
    )]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, help = "Overwrite locally modified bundled skill files")]
    force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillStatusArgs {
    #[arg(
        long = "agent",
        value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents"
    )]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Check the current project's skill dirs instead of global dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

impl SkillInstallArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(
            telemetry,
            self.agent.len(),
            self.all_agents,
            self.project,
            self.force,
        );
    }
}

impl SkillStatusArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(
            telemetry,
            self.agent.len(),
            self.all_agents,
            self.project,
            false,
        );
    }
}

pub(crate) fn run_install_command(
    args: SkillInstallArgs,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_install(args, &context, telemetry, ui)
}

pub(crate) fn run_status_command(
    args: SkillStatusArgs,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_status(args, &context, telemetry, ui)
}

fn insert_target_analytics(
    telemetry: &mut IntegrationTelemetry,
    explicit_agents: usize,
    all_agents: bool,
    project: bool,
    force: bool,
) {
    telemetry.scope = Some(if project {
        IntegrationScope::Project
    } else {
        IntegrationScope::Global
    });
    telemetry.selection = Some(if all_agents {
        TargetSelection::All
    } else if explicit_agents == 0 {
        TargetSelection::Fallback
    } else {
        TargetSelection::Explicit
    });
    telemetry.target_agents = Some(count_bucket(if all_agents {
        SkillAgentArg::ALL.len() as u64
    } else {
        explicit_agents.max(1) as u64
    }));
    telemetry.force = Some(force);
}
