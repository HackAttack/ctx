use std::io::{self, IsTerminal, Write};

use anyhow::{anyhow, Context, Result};
use ctx_agent_application::skill::{
    complete_picker_selection, plan_install_selection, status_selection, SkillInstallSelectionPlan,
    SkillPickerPrompt, SkillSelectionRequest,
};
use ctx_agent_integrations::skill::{parse_picker_selection, SkillAgentSelection};

use super::{
    agents::{picker_agents, SkillAgentArg},
    paths::PathContext,
    SkillInstallArgs, SkillStatusArgs,
};

pub(super) fn install_agent_selection(
    args: &SkillInstallArgs,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    match plan_install_selection(
        SkillSelectionRequest {
            agents: &args.agent,
            all_agents: args.all_agents,
            allow_picker: !args.format.is_json() && can_prompt(),
        },
        context,
    )? {
        SkillInstallSelectionPlan::Selected(selection) => Ok(selection),
        SkillInstallSelectionPlan::Prompt(prompt) => {
            Ok(complete_picker_selection(prompt_for_agents(&prompt)?))
        }
    }
}

pub(super) fn status_agent_selection(
    args: &SkillStatusArgs,
    context: &PathContext,
) -> SkillAgentSelection {
    status_selection(&args.agent, args.all_agents, context)
}

fn can_prompt() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn prompt_for_agents(prompt: &SkillPickerPrompt) -> Result<Vec<SkillAgentArg>> {
    let options = picker_agents();
    let defaults = prompt
        .options
        .iter()
        .filter(|option| option.selected_by_default)
        .map(|option| option.agent)
        .collect::<Vec<_>>();
    let mut stderr = crate::output::stderr_writer();
    for line in picker_prompt_lines(prompt) {
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

fn picker_prompt_lines(prompt: &SkillPickerPrompt) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Select where to install {}. Detected agents are preselected.",
            prompt.skill_name
        ),
        "Press Enter for the marked defaults, or enter numbers like 1,2.".to_owned(),
    ];
    for (index, option) in prompt.options.iter().enumerate() {
        let marker = if option.selected_by_default { "*" } else { " " };
        let detected_hint = if option.detected { " detected" } else { "" };
        lines.push(format!(
            "  {}. [{}] {} -> {}{}",
            index + 1,
            marker,
            option.agent.display_name(),
            option.target.skill_dir.display(),
            detected_hint
        ));
    }
    lines
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn interactive_picker_prompt_is_explicit_and_actionable() {
        let temp = tempfile::tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let SkillInstallSelectionPlan::Prompt(prompt) = plan_install_selection(
            SkillSelectionRequest {
                agents: &[],
                all_agents: false,
                allow_picker: true,
            },
            &context,
        )
        .unwrap() else {
            panic!("interactive selection should request a prompt");
        };
        let lines = picker_prompt_lines(&prompt);
        let rendered = lines.join("\n");

        assert!(rendered.contains("Select where to install"));
        assert!(rendered.contains("Press Enter for the marked defaults"));
        assert!(rendered.contains("[*] Universal"));
        assert!(rendered.contains(".agents/skills/ctx-agent-history-search"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
