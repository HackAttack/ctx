use std::io::{self, IsTerminal, Write};

use anyhow::{anyhow, Context, Result};
use ctx_agent_integrations::skill::{
    default_agent_selection, detected_agents, explicit_agent_selection, parse_picker_selection,
    picker_agent_selection, SkillAgentSelection,
};

use super::{
    agents::{picker_agents, SkillAgentArg},
    paths::PathContext,
    target::single_target,
    SkillInstallArgs, SkillStatusArgs, BUNDLED_SKILL_NAME,
};

pub(super) fn default_picker_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    default_agent_selection(context).agents
}

pub(super) fn install_agent_selection(
    args: &SkillInstallArgs,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    if let Some(selection) = explicit_agent_selection(&args.agent, args.all_agents) {
        return Ok(selection);
    }
    if args.format.is_json() || !can_prompt() {
        return Ok(default_agent_selection(context));
    }
    Ok(picker_agent_selection(prompt_for_agents(context)?))
}

pub(super) fn status_agent_selection(
    args: &SkillStatusArgs,
    context: &PathContext,
) -> SkillAgentSelection {
    explicit_agent_selection(&args.agent, args.all_agents)
        .unwrap_or_else(|| default_agent_selection(context))
}

fn can_prompt() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn prompt_for_agents(context: &PathContext) -> Result<Vec<SkillAgentArg>> {
    let options = picker_agents();
    let detected = detected_agents(context);
    let defaults = default_picker_agents(context);
    let mut stderr = crate::output::stderr_writer();
    for line in picker_prompt_lines(context, options, &detected, &defaults)? {
        writeln!(stderr, "{line}")?;
    }
    loop {
        write!(stderr, "Install target(s): ")?;
        stderr.flush()?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read skill install selection")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(defaults);
        }
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "q" | "quit" | "cancel"
        ) {
            return Err(anyhow!("skill install canceled"));
        }
        match parse_picker_selection(trimmed, options) {
            Ok(agents) => return Ok(agents),
            Err(err) => {
                writeln!(stderr, "{err}")?;
            }
        }
    }
}

fn picker_prompt_lines(
    context: &PathContext,
    options: &[SkillAgentArg],
    detected: &[SkillAgentArg],
    defaults: &[SkillAgentArg],
) -> Result<Vec<String>> {
    let mut lines = vec![
        format!("Select where to install {BUNDLED_SKILL_NAME}. Detected agents are preselected."),
        "Press Enter for the marked defaults, or enter numbers like 1,2.".to_owned(),
    ];
    for (index, agent) in options.iter().enumerate() {
        let marker = if defaults.contains(agent) { "*" } else { " " };
        let detected_hint = if detected.contains(agent) {
            " detected"
        } else {
            ""
        };
        let target = single_target(*agent, false, context)?;
        lines.push(format!(
            "  {}. [{}] {} -> {}{}",
            index + 1,
            marker,
            agent.display_name(),
            target.skill_dir.display(),
            detected_hint
        ));
    }
    Ok(lines)
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn interactive_picker_prompt_is_explicit_and_actionable() {
        let temp = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let options = picker_agents();
        let defaults = vec![SkillAgentArg::Universal];
        let lines = picker_prompt_lines(&context, options, &[], &defaults).unwrap();
        let rendered = lines.join("\n");

        assert!(rendered.contains("Select where to install"));
        assert!(rendered.contains("Press Enter for the marked defaults"));
        assert!(rendered.contains("[*] Universal"));
        assert!(rendered.contains(".agents/skills/ctx-agent-history-search"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
