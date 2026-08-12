use super::{SkillAgentArg, SkillInstallArgs};
use crate::{
    analytics::{IntegrationScope, IntegrationTelemetry, TargetSelection},
    output::JsonOutputFormat,
};

#[test]
fn initial_argument_telemetry_stays_cli_owned_and_path_free() {
    let args = SkillInstallArgs {
        agent: vec![SkillAgentArg::Codex, SkillAgentArg::ClaudeCode],
        all_agents: false,
        project: true,
        format: JsonOutputFormat::Json,
        force: false,
    };
    let mut telemetry = IntegrationTelemetry::default();
    args.add_initial_analytics(&mut telemetry);

    assert_eq!(telemetry.scope, Some(IntegrationScope::Project));
    assert_eq!(telemetry.selection, Some(TargetSelection::Explicit));
    assert_eq!(telemetry.force, Some(false));
}
